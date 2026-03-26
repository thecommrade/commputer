use std::collections::HashMap;
use serde::{Deserialize, Serialize};

/// Per-holder usage statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HolderUsageStats {
    pub address_hex: String,
    pub total_comme_spent: u64,
    pub total_jobs_submitted: u64,
    pub total_jobs_completed: u64,
    pub avg_job_duration_secs: f64,
    pub total_results_received: u64,
}

/// Aggregated usage analytics across all holders.
pub struct UsageAnalytics {
    pub stats: HashMap<String, HolderUsageStats>,
}

impl Default for UsageAnalytics {
    fn default() -> Self {
        Self::new()
    }
}

impl UsageAnalytics {
    pub fn new() -> Self {
        Self {
            stats: HashMap::new(),
        }
    }

    /// Record a new job submission for a holder.
    pub fn record_submission(&mut self, address_hex: &str, comme_budget: u64) {
        let entry = self
            .stats
            .entry(address_hex.to_string())
            .or_insert_with(|| HolderUsageStats {
                address_hex: address_hex.to_string(),
                total_comme_spent: 0,
                total_jobs_submitted: 0,
                total_jobs_completed: 0,
                avg_job_duration_secs: 0.0,
                total_results_received: 0,
            });
        entry.total_jobs_submitted += 1;
        entry.total_comme_spent += comme_budget;
    }

    /// Record a completed job for a holder.
    pub fn record_completion(&mut self, address_hex: &str, duration_secs: f64) {
        if let Some(entry) = self.stats.get_mut(address_hex) {
            let prev_total = entry.avg_job_duration_secs * entry.total_jobs_completed as f64;
            entry.total_jobs_completed += 1;
            entry.total_results_received += 1;
            entry.avg_job_duration_secs =
                (prev_total + duration_secs) / entry.total_jobs_completed as f64;
        }
    }

    /// Get stats for a specific holder.
    pub fn get_stats(&self, address_hex: &str) -> Option<&HolderUsageStats> {
        self.stats.get(address_hex)
    }

    /// Get the top N users by total COMME spent.
    pub fn top_users(&self, n: usize) -> Vec<&HolderUsageStats> {
        let mut users: Vec<&HolderUsageStats> = self.stats.values().collect();
        users.sort_by(|a, b| b.total_comme_spent.cmp(&a.total_comme_spent));
        users.truncate(n);
        users
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_submission() {
        let mut analytics = UsageAnalytics::new();
        analytics.record_submission("alice", 100_000_000);
        analytics.record_submission("alice", 200_000_000);

        let stats = analytics.get_stats("alice").unwrap();
        assert_eq!(stats.total_jobs_submitted, 2);
        assert_eq!(stats.total_comme_spent, 300_000_000);
    }

    #[test]
    fn test_record_completion() {
        let mut analytics = UsageAnalytics::new();
        analytics.record_submission("bob", 50_000_000);
        analytics.record_completion("bob", 10.0);
        analytics.record_completion("bob", 20.0);

        let stats = analytics.get_stats("bob").unwrap();
        assert_eq!(stats.total_jobs_completed, 2);
        assert!((stats.avg_job_duration_secs - 15.0).abs() < 0.01);
    }

    #[test]
    fn test_top_users() {
        let mut analytics = UsageAnalytics::new();
        analytics.record_submission("alice", 100);
        analytics.record_submission("bob", 300);
        analytics.record_submission("charlie", 200);

        let top = analytics.top_users(2);
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].address_hex, "bob");
        assert_eq!(top[1].address_hex, "charlie");
    }

    #[test]
    fn test_nonexistent_user() {
        let analytics = UsageAnalytics::new();
        assert!(analytics.get_stats("nobody").is_none());
    }
}
