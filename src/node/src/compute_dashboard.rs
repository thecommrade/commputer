#![allow(dead_code)]
use serde::{Deserialize, Serialize};

/// Network-wide compute dashboard metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComputeDashboard {
    pub total_jobs_processed: u64,
    pub total_comme_burned_on_compute: u64,
    pub avg_job_price: f64,
    pub capacity_utilization_pct: f64,
    pub jobs_per_epoch: u64,
    pub flagship_jobs_pct: f64,
}

impl ComputeDashboard {
    /// Create a new empty dashboard.
    pub fn new() -> Self {
        Self {
            total_jobs_processed: 0,
            total_comme_burned_on_compute: 0,
            avg_job_price: 0.0,
            capacity_utilization_pct: 0.0,
            jobs_per_epoch: 0,
            flagship_jobs_pct: 0.0,
        }
    }

    /// Update the dashboard with a newly completed job.
    pub fn update_dashboard(&mut self, new_job_price: u64, _new_job_completed: bool) {
        self.total_jobs_processed += 1;
        self.total_comme_burned_on_compute += new_job_price;
        self.avg_job_price =
            self.total_comme_burned_on_compute as f64 / self.total_jobs_processed as f64;
    }

    /// Set the current capacity utilization.
    pub fn set_utilization(&mut self, pct: f64) {
        self.capacity_utilization_pct = pct;
    }

    /// Set the flagship jobs percentage.
    pub fn set_flagship_pct(&mut self, pct: f64) {
        self.flagship_jobs_pct = pct;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_dashboard() {
        let d = ComputeDashboard::new();
        assert_eq!(d.total_jobs_processed, 0);
        assert_eq!(d.total_comme_burned_on_compute, 0);
    }

    #[test]
    fn test_update_dashboard() {
        let mut d = ComputeDashboard::new();
        d.update_dashboard(100_000, true);
        assert_eq!(d.total_jobs_processed, 1);
        assert_eq!(d.total_comme_burned_on_compute, 100_000);
        assert!((d.avg_job_price - 100_000.0).abs() < 0.01);

        d.update_dashboard(200_000, true);
        assert_eq!(d.total_jobs_processed, 2);
        assert_eq!(d.total_comme_burned_on_compute, 300_000);
        assert!((d.avg_job_price - 150_000.0).abs() < 0.01);
    }

    #[test]
    fn test_utilization() {
        let mut d = ComputeDashboard::new();
        d.set_utilization(45.5);
        assert!((d.capacity_utilization_pct - 45.5).abs() < 0.01);
    }

    #[test]
    fn test_flagship_pct() {
        let mut d = ComputeDashboard::new();
        d.set_flagship_pct(51.0);
        assert!((d.flagship_jobs_pct - 51.0).abs() < 0.01);
    }
}
