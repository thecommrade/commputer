use std::collections::{BTreeMap, HashMap};
use serde::{Deserialize, Serialize};
use commputer_core::identity::Address;

/// Unique job identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct JobId(pub [u8; 32]);

impl PartialOrd for JobId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for JobId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.cmp(&other.0)
    }
}

/// Status of a compute job in the pool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PoolJobStatus {
    Pending,
    Assigned {
        executor: Address,
        assigned_height: u64,
    },
    Running {
        executor: Address,
        started_height: u64,
    },
    Completed {
        executor: Address,
        result_hash: [u8; 32],
        completed_height: u64,
    },
    Failed {
        reason: String,
    },
    Disputed {
        challenger: Address,
    },
}

/// A job entry in the pool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolJob {
    pub job_id: JobId,
    pub submitter: Address,
    pub comme_budget: u64,
    pub cpu_cores: u16,
    pub gpu_vram_mb: u64,
    pub ram_mb: u64,
    pub storage_mb: u64,
    pub bandwidth_mbps: u64,
    pub max_duration_secs: u64,
    pub job_spec_hash: [u8; 32],
    pub status: PoolJobStatus,
    pub submitted_height: u64,
    pub l2_id: Option<String>,
}

/// The job pool — manages pending, active, and completed jobs.
pub struct JobPool {
    /// All jobs indexed by JobId.
    jobs: HashMap<JobId, PoolJob>,
    /// Pending jobs sorted by budget (highest first) for assignment.
    pending_by_budget: BTreeMap<(std::cmp::Reverse<u64>, JobId), ()>,
    /// Jobs assigned to each validator.
    assigned_to_validator: HashMap<Address, Vec<JobId>>,
    /// Completed job count.
    completed_count: u64,
    /// Failed job count.
    failed_count: u64,
}

impl JobPool {
    pub fn new() -> Self {
        Self {
            jobs: HashMap::new(),
            pending_by_budget: BTreeMap::new(),
            assigned_to_validator: HashMap::new(),
            completed_count: 0,
            failed_count: 0,
        }
    }

    /// Add a new pending job.
    pub fn submit_job(&mut self, job: PoolJob) {
        let id = job.job_id;
        let budget = job.comme_budget;
        self.pending_by_budget
            .insert((std::cmp::Reverse(budget), id), ());
        self.jobs.insert(id, job);
    }

    /// Get a job by ID.
    pub fn get(&self, id: &JobId) -> Option<&PoolJob> {
        self.jobs.get(id)
    }

    /// Get a mutable job by ID.
    pub fn get_mut(&mut self, id: &JobId) -> Option<&mut PoolJob> {
        self.jobs.get_mut(id)
    }

    /// Get all pending jobs sorted by budget (highest first).
    pub fn pending_jobs(&self) -> Vec<&PoolJob> {
        self.pending_by_budget
            .keys()
            .filter_map(|(_, id)| self.jobs.get(id))
            .filter(|j| matches!(j.status, PoolJobStatus::Pending))
            .collect()
    }

    /// Get pending flagship L2 jobs (for 51% priority).
    pub fn pending_flagship_jobs(&self) -> Vec<&PoolJob> {
        self.pending_jobs()
            .into_iter()
            .filter(|j| j.l2_id.as_deref() == Some("commputer-analytics-l2"))
            .collect()
    }

    /// Get pending non-flagship jobs (49% capacity).
    pub fn pending_other_jobs(&self) -> Vec<&PoolJob> {
        self.pending_jobs()
            .into_iter()
            .filter(|j| j.l2_id.as_deref() != Some("commputer-analytics-l2"))
            .collect()
    }

    /// Assign a job to a validator.
    pub fn assign_job(&mut self, job_id: &JobId, executor: Address, height: u64) -> bool {
        if let Some(job) = self.jobs.get_mut(job_id) {
            if !matches!(job.status, PoolJobStatus::Pending) {
                return false;
            }
            job.status = PoolJobStatus::Assigned {
                executor,
                assigned_height: height,
            };
            self.pending_by_budget
                .remove(&(std::cmp::Reverse(job.comme_budget), *job_id));
            self.assigned_to_validator
                .entry(executor)
                .or_default()
                .push(*job_id);
            true
        } else {
            false
        }
    }

    /// Complete a job.
    pub fn complete_job(&mut self, job_id: &JobId, result_hash: [u8; 32], height: u64) -> bool {
        if let Some(job) = self.jobs.get_mut(job_id) {
            if let PoolJobStatus::Assigned { executor, .. }
            | PoolJobStatus::Running { executor, .. } = job.status
            {
                job.status = PoolJobStatus::Completed {
                    executor,
                    result_hash,
                    completed_height: height,
                };
                self.completed_count += 1;
                true
            } else {
                false
            }
        } else {
            false
        }
    }

    /// Dispute a job.
    pub fn dispute_job(&mut self, job_id: &JobId, challenger: Address) -> bool {
        if let Some(job) = self.jobs.get_mut(job_id) {
            if matches!(job.status, PoolJobStatus::Completed { .. }) {
                job.status = PoolJobStatus::Disputed { challenger };
                true
            } else {
                false
            }
        } else {
            false
        }
    }

    /// Fail a job (timeout or error).
    pub fn fail_job(&mut self, job_id: &JobId, reason: String) -> bool {
        if let Some(job) = self.jobs.get_mut(job_id) {
            job.status = PoolJobStatus::Failed { reason };
            self.failed_count += 1;
            true
        } else {
            false
        }
    }

    /// Cancel a pending job. Returns the comme_budget for partial refund.
    pub fn cancel_job(&mut self, job_id: &JobId) -> Option<u64> {
        if let Some(job) = self.jobs.get(job_id) {
            if !matches!(job.status, PoolJobStatus::Pending) {
                return None;
            }
            let budget = job.comme_budget;
            self.pending_by_budget
                .remove(&(std::cmp::Reverse(budget), *job_id));
            self.jobs.remove(job_id);
            Some(budget)
        } else {
            None
        }
    }

    /// Get jobs assigned to a specific validator.
    pub fn validator_jobs(&self, validator: &Address) -> Vec<&PoolJob> {
        self.assigned_to_validator
            .get(validator)
            .map(|ids| ids.iter().filter_map(|id| self.jobs.get(id)).collect())
            .unwrap_or_default()
    }

    /// Get timed-out jobs (assigned but not completed within max_duration).
    pub fn timed_out_jobs(&self, current_height: u64, secs_per_block: u64) -> Vec<JobId> {
        self.jobs
            .values()
            .filter(|j| {
                if let PoolJobStatus::Assigned {
                    assigned_height, ..
                } = j.status
                {
                    let elapsed_blocks = current_height.saturating_sub(assigned_height);
                    let elapsed_secs = elapsed_blocks * secs_per_block;
                    elapsed_secs > j.max_duration_secs
                } else {
                    false
                }
            })
            .map(|j| j.job_id)
            .collect()
    }

    /// Enforce timeouts: mark timed-out jobs as failed and return them to pending.
    /// Returns a list of (JobId, executor Address) for reputation penalties.
    pub fn enforce_timeouts(
        &mut self,
        current_height: u64,
        secs_per_block: u64,
    ) -> Vec<(JobId, Address)> {
        let timed_out = self.timed_out_jobs(current_height, secs_per_block);
        let mut penalties = Vec::new();

        for job_id in timed_out {
            if let Some(job) = self.jobs.get_mut(&job_id)
                && let PoolJobStatus::Assigned { executor, .. } = job.status {
                    penalties.push((job_id, executor));
                    // Reset to pending so it can be reassigned
                    job.status = PoolJobStatus::Pending;
                    self.pending_by_budget
                        .insert((std::cmp::Reverse(job.comme_budget), job_id), ());
                    // Remove from assigned_to_validator
                    if let Some(assigned) = self.assigned_to_validator.get_mut(&executor) {
                        assigned.retain(|id| *id != job_id);
                    }
                }
        }

        penalties
    }

    /// Total pending jobs.
    pub fn pending_count(&self) -> usize {
        self.pending_by_budget.len()
    }

    /// Total active (assigned/running) jobs.
    pub fn active_count(&self) -> usize {
        self.jobs
            .values()
            .filter(|j| {
                matches!(
                    j.status,
                    PoolJobStatus::Assigned { .. } | PoolJobStatus::Running { .. }
                )
            })
            .count()
    }

    /// Total completed jobs.
    pub fn completed_count(&self) -> u64 {
        self.completed_count
    }

    /// Total failed jobs.
    pub fn failed_count(&self) -> u64 {
        self.failed_count
    }

    /// Total jobs.
    pub fn total_count(&self) -> usize {
        self.jobs.len()
    }

    /// Get recent completed jobs (last N).
    pub fn recent_completed(&self, limit: usize) -> Vec<&PoolJob> {
        let mut completed: Vec<&PoolJob> = self
            .jobs
            .values()
            .filter(|j| matches!(j.status, PoolJobStatus::Completed { .. }))
            .collect();
        completed.sort_by(|a, b| {
            let h_a = if let PoolJobStatus::Completed {
                completed_height, ..
            } = a.status
            {
                completed_height
            } else {
                0
            };
            let h_b = if let PoolJobStatus::Completed {
                completed_height, ..
            } = b.status
            {
                completed_height
            } else {
                0
            };
            h_b.cmp(&h_a)
        });
        completed.truncate(limit);
        completed
    }

    /// All jobs as a list.
    pub fn all_jobs(&self) -> Vec<&PoolJob> {
        self.jobs.values().collect()
    }

    /// Route jobs respecting the 51/49 split and dynamic reserve.
    /// Returns (flagship_jobs, user_jobs) that can be assigned given capacity constraints.
    /// `total_capacity` is total available compute slots.
    /// `churn_rate` is validator churn for dynamic reserve calculation.
    pub fn route_jobs_with_capacity(
        &self,
        total_capacity: u64,
        churn_rate: f64,
    ) -> (Vec<&PoolJob>, Vec<&PoolJob>) {
        use commputer_core::token::{
            dynamic_reserve_percent, FLAGSHIP_COMPUTE_SHARE, HOLDER_COMPUTE_SHARE,
        };

        let reserve_pct = dynamic_reserve_percent(churn_rate);
        let usable_capacity = total_capacity.saturating_sub(total_capacity * reserve_pct / 100);
        let flagship_capacity = usable_capacity * FLAGSHIP_COMPUTE_SHARE / 100;
        let user_capacity = usable_capacity * HOLDER_COMPUTE_SHARE / 100;

        let flagship_jobs: Vec<&PoolJob> = self
            .pending_flagship_jobs()
            .into_iter()
            .take(flagship_capacity as usize)
            .collect();

        let user_jobs: Vec<&PoolJob> = self
            .pending_other_jobs()
            .into_iter()
            .take(user_capacity as usize)
            .collect();

        (flagship_jobs, user_jobs)
    }

    /// Get current capacity breakdown for RPC reporting.
    /// Returns (total, reserve_pct, flagship_slots, user_slots).
    pub fn capacity_breakdown(
        &self,
        total_capacity: u64,
        churn_rate: f64,
    ) -> (u64, u64, u64, u64) {
        use commputer_core::token::{
            dynamic_reserve_percent, FLAGSHIP_COMPUTE_SHARE, HOLDER_COMPUTE_SHARE,
        };

        let reserve_pct = dynamic_reserve_percent(churn_rate);
        let usable = total_capacity.saturating_sub(total_capacity * reserve_pct / 100);
        let flagship = usable * FLAGSHIP_COMPUTE_SHARE / 100;
        let user = usable * HOLDER_COMPUTE_SHARE / 100;
        (total_capacity, reserve_pct, flagship, user)
    }
}

/// Serializable snapshot of the job pool for persistence.
#[derive(Serialize, Deserialize)]
struct JobPoolSnapshot {
    jobs: Vec<PoolJob>,
    completed_count: u64,
    failed_count: u64,
}

impl JobPool {
    /// Serialize the job pool to JSON bytes for persistence.
    pub fn to_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        let snapshot = JobPoolSnapshot {
            jobs: self.jobs.values().cloned().collect(),
            completed_count: self.completed_count,
            failed_count: self.failed_count,
        };
        serde_json::to_vec_pretty(&snapshot)
    }

    /// Deserialize a job pool from JSON bytes.
    pub fn from_json(data: &[u8]) -> Result<Self, serde_json::Error> {
        let snapshot: JobPoolSnapshot = serde_json::from_slice(data)?;
        let mut pool = Self::new();
        pool.completed_count = snapshot.completed_count;
        pool.failed_count = snapshot.failed_count;
        for job in snapshot.jobs {
            let id = job.job_id;
            let budget = job.comme_budget;
            if matches!(job.status, PoolJobStatus::Pending) {
                pool.pending_by_budget
                    .insert((std::cmp::Reverse(budget), id), ());
            }
            if let PoolJobStatus::Assigned { executor, .. }
                | PoolJobStatus::Running { executor, .. } = job.status
            {
                pool.assigned_to_validator
                    .entry(executor)
                    .or_default()
                    .push(id);
            }
            pool.jobs.insert(id, job);
        }
        Ok(pool)
    }

    /// Save the job pool to a file in the given directory.
    pub fn save_to_dir(&self, dir: &std::path::Path) -> std::io::Result<()> {
        let path = dir.join("job_pool.json");
        let data = self.to_json().map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        std::fs::write(path, data)
    }

    /// Load the job pool from a file in the given directory.
    pub fn load_from_dir(dir: &std::path::Path) -> std::io::Result<Self> {
        let path = dir.join("job_pool.json");
        let data = std::fs::read(&path)?;
        Self::from_json(&data).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
    }
}

impl Default for JobPool {
    fn default() -> Self {
        Self::new()
    }
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

    fn make_job(id_byte: u8, budget: u64, l2_id: Option<&str>) -> PoolJob {
        PoolJob {
            job_id: make_job_id(id_byte),
            submitter: make_address(0xFF),
            comme_budget: budget,
            cpu_cores: 4,
            gpu_vram_mb: 8192,
            ram_mb: 16384,
            storage_mb: 102400,
            bandwidth_mbps: 1000,
            max_duration_secs: 3600,
            job_spec_hash: [id_byte; 32],
            status: PoolJobStatus::Pending,
            submitted_height: 100,
            l2_id: l2_id.map(|s| s.to_string()),
        }
    }

    #[test]
    fn test_submit_and_get() {
        let mut pool = JobPool::new();
        let job = make_job(1, 1000, None);
        pool.submit_job(job.clone());

        assert_eq!(pool.total_count(), 1);
        assert_eq!(pool.pending_count(), 1);
        let retrieved = pool.get(&make_job_id(1)).unwrap();
        assert_eq!(retrieved.comme_budget, 1000);
    }

    #[test]
    fn test_pending_sorted_by_budget() {
        let mut pool = JobPool::new();
        pool.submit_job(make_job(1, 500, None));
        pool.submit_job(make_job(2, 1000, None));
        pool.submit_job(make_job(3, 750, None));

        let pending = pool.pending_jobs();
        assert_eq!(pending.len(), 3);
        assert_eq!(pending[0].comme_budget, 1000);
        assert_eq!(pending[1].comme_budget, 750);
        assert_eq!(pending[2].comme_budget, 500);
    }

    #[test]
    fn test_assign_job() {
        let mut pool = JobPool::new();
        pool.submit_job(make_job(1, 1000, None));

        let executor = make_address(0x01);
        assert!(pool.assign_job(&make_job_id(1), executor, 200));
        assert_eq!(pool.pending_count(), 0);
        assert_eq!(pool.active_count(), 1);

        let job = pool.get(&make_job_id(1)).unwrap();
        assert!(matches!(
            job.status,
            PoolJobStatus::Assigned {
                assigned_height: 200,
                ..
            }
        ));
    }

    #[test]
    fn test_assign_non_pending_fails() {
        let mut pool = JobPool::new();
        pool.submit_job(make_job(1, 1000, None));

        let executor = make_address(0x01);
        assert!(pool.assign_job(&make_job_id(1), executor, 200));
        // Second assign should fail
        assert!(!pool.assign_job(&make_job_id(1), make_address(0x02), 201));
    }

    #[test]
    fn test_complete_job() {
        let mut pool = JobPool::new();
        pool.submit_job(make_job(1, 1000, None));
        let executor = make_address(0x01);
        pool.assign_job(&make_job_id(1), executor, 200);

        let result_hash = [0xAA; 32];
        assert!(pool.complete_job(&make_job_id(1), result_hash, 300));
        assert_eq!(pool.completed_count(), 1);
        assert_eq!(pool.active_count(), 0);
    }

    #[test]
    fn test_complete_pending_fails() {
        let mut pool = JobPool::new();
        pool.submit_job(make_job(1, 1000, None));
        assert!(!pool.complete_job(&make_job_id(1), [0; 32], 300));
    }

    #[test]
    fn test_dispute_job() {
        let mut pool = JobPool::new();
        pool.submit_job(make_job(1, 1000, None));
        pool.assign_job(&make_job_id(1), make_address(0x01), 200);
        pool.complete_job(&make_job_id(1), [0xAA; 32], 300);

        let challenger = make_address(0x02);
        assert!(pool.dispute_job(&make_job_id(1), challenger));
        let job = pool.get(&make_job_id(1)).unwrap();
        assert!(matches!(job.status, PoolJobStatus::Disputed { .. }));
    }

    #[test]
    fn test_dispute_non_completed_fails() {
        let mut pool = JobPool::new();
        pool.submit_job(make_job(1, 1000, None));
        assert!(!pool.dispute_job(&make_job_id(1), make_address(0x02)));
    }

    #[test]
    fn test_cancel_pending_job() {
        let mut pool = JobPool::new();
        pool.submit_job(make_job(1, 1000, None));

        let refund = pool.cancel_job(&make_job_id(1));
        assert_eq!(refund, Some(1000));
        assert_eq!(pool.total_count(), 0);
        assert_eq!(pool.pending_count(), 0);
    }

    #[test]
    fn test_cancel_non_pending_fails() {
        let mut pool = JobPool::new();
        pool.submit_job(make_job(1, 1000, None));
        pool.assign_job(&make_job_id(1), make_address(0x01), 200);

        assert_eq!(pool.cancel_job(&make_job_id(1)), None);
    }

    #[test]
    fn test_fail_job() {
        let mut pool = JobPool::new();
        pool.submit_job(make_job(1, 1000, None));
        assert!(pool.fail_job(&make_job_id(1), "out of memory".to_string()));
        assert_eq!(pool.failed_count(), 1);
    }

    #[test]
    fn test_validator_jobs() {
        let mut pool = JobPool::new();
        let executor = make_address(0x01);
        pool.submit_job(make_job(1, 1000, None));
        pool.submit_job(make_job(2, 2000, None));
        pool.assign_job(&make_job_id(1), executor, 200);
        pool.assign_job(&make_job_id(2), executor, 201);

        let jobs = pool.validator_jobs(&executor);
        assert_eq!(jobs.len(), 2);
    }

    #[test]
    fn test_timed_out_jobs() {
        let mut pool = JobPool::new();
        let mut job = make_job(1, 1000, None);
        job.max_duration_secs = 100;
        pool.submit_job(job);
        pool.assign_job(&make_job_id(1), make_address(0x01), 10);

        // At height 20 with 10 sec/block = 100 secs elapsed, not timed out (need > 100)
        assert!(pool.timed_out_jobs(20, 10).is_empty());
        // At height 21 with 10 sec/block = 110 secs elapsed, timed out
        let timed_out = pool.timed_out_jobs(21, 10);
        assert_eq!(timed_out.len(), 1);
        assert_eq!(timed_out[0], make_job_id(1));
    }

    #[test]
    fn test_enforce_timeouts() {
        let mut pool = JobPool::new();
        let executor = make_address(0x01);
        let mut job = make_job(1, 1000, None);
        job.max_duration_secs = 100;
        pool.submit_job(job);
        pool.assign_job(&make_job_id(1), executor, 10);

        let penalties = pool.enforce_timeouts(21, 10);
        assert_eq!(penalties.len(), 1);
        assert_eq!(penalties[0], (make_job_id(1), executor));

        // Job should be back to pending
        assert_eq!(pool.pending_count(), 1);
        assert_eq!(pool.active_count(), 0);
        let job = pool.get(&make_job_id(1)).unwrap();
        assert!(matches!(job.status, PoolJobStatus::Pending));
    }

    #[test]
    fn test_flagship_vs_other_priority() {
        let mut pool = JobPool::new();
        pool.submit_job(make_job(1, 1000, Some("commputer-analytics-l2")));
        pool.submit_job(make_job(2, 2000, Some("other-l2")));
        pool.submit_job(make_job(3, 500, Some("commputer-analytics-l2")));
        pool.submit_job(make_job(4, 3000, None));

        let flagship = pool.pending_flagship_jobs();
        assert_eq!(flagship.len(), 2);
        // Should be sorted by budget (highest first)
        assert_eq!(flagship[0].comme_budget, 1000);
        assert_eq!(flagship[1].comme_budget, 500);

        let other = pool.pending_other_jobs();
        assert_eq!(other.len(), 2);
    }

    #[test]
    fn test_recent_completed() {
        let mut pool = JobPool::new();
        for i in 0..5u8 {
            pool.submit_job(make_job(i, 1000, None));
            pool.assign_job(&make_job_id(i), make_address(0x01), 100 + i as u64);
            pool.complete_job(&make_job_id(i), [i; 32], 200 + i as u64);
        }

        let recent = pool.recent_completed(3);
        assert_eq!(recent.len(), 3);
        // Most recent first
        if let PoolJobStatus::Completed {
            completed_height, ..
        } = recent[0].status
        {
            assert_eq!(completed_height, 204);
        }
    }

    #[test]
    fn test_get_nonexistent() {
        let pool = JobPool::new();
        assert!(pool.get(&make_job_id(99)).is_none());
    }

    #[test]
    fn test_assign_nonexistent_fails() {
        let mut pool = JobPool::new();
        assert!(!pool.assign_job(&make_job_id(99), make_address(0x01), 100));
    }
}
