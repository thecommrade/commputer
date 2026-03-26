#![allow(dead_code)]
use std::collections::HashMap;

/// Rate limiter that enforces per-tier job submission limits.
pub struct TierRateLimiter {
    /// address -> (submission_count, epoch)
    pub limits: HashMap<String, (u64, u64)>,
}

/// Maximum jobs per epoch by balance tier.
pub fn max_jobs_for_balance(balance: u64) -> u64 {
    // Tier thresholds (raw units, 1 COMME = 100_000_000)
    const TIER_STORAGE: u64 = 100_000_000;
    const TIER_COMPUTE: u64 = 1_000_000_000;
    const TIER_FULL: u64 = 2_000_000_000;
    const TIER_UNLIMITED: u64 = 3_300_000_000;

    if balance >= TIER_UNLIMITED {
        200
    } else if balance >= TIER_FULL {
        50
    } else if balance >= TIER_COMPUTE {
        20
    } else if balance >= TIER_STORAGE {
        5
    } else {
        0 // ReadOnly or None
    }
}

impl TierRateLimiter {
    pub fn new() -> Self {
        Self {
            limits: HashMap::new(),
        }
    }

    /// Check if the address is under its rate limit for the current epoch.
    pub fn check_rate_limit(&self, address: &str, balance: u64, current_epoch: u64) -> bool {
        let max = max_jobs_for_balance(balance);
        if max == 0 {
            return false;
        }
        match self.limits.get(address) {
            Some((count, epoch)) if *epoch == current_epoch => *count < max,
            _ => true, // new epoch or first submission
        }
    }

    /// Record a job submission for the given address.
    pub fn record_submission(&mut self, address: &str, current_epoch: u64) {
        let entry = self
            .limits
            .entry(address.to_string())
            .or_insert((0, current_epoch));

        if entry.1 != current_epoch {
            // New epoch, reset counter
            entry.0 = 1;
            entry.1 = current_epoch;
        } else {
            entry.0 += 1;
        }
    }

    /// Get the current count for an address in a given epoch.
    pub fn current_count(&self, address: &str, epoch: u64) -> u64 {
        match self.limits.get(address) {
            Some((count, e)) if *e == epoch => *count,
            _ => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_max_jobs_tiers() {
        assert_eq!(max_jobs_for_balance(0), 0);
        assert_eq!(max_jobs_for_balance(50_000_000), 0);
        assert_eq!(max_jobs_for_balance(100_000_000), 5);
        assert_eq!(max_jobs_for_balance(1_000_000_000), 20);
        assert_eq!(max_jobs_for_balance(2_000_000_000), 50);
        assert_eq!(max_jobs_for_balance(3_300_000_000), 200);
    }

    #[test]
    fn test_rate_limit_allows_first() {
        let limiter = TierRateLimiter::new();
        // Storage tier (1 COMME) can submit 5 per epoch
        assert!(limiter.check_rate_limit("alice", 100_000_000, 1));
    }

    #[test]
    fn test_rate_limit_blocks_zero_balance() {
        let limiter = TierRateLimiter::new();
        assert!(!limiter.check_rate_limit("broke_user", 0, 1));
    }

    #[test]
    fn test_rate_limit_exhaustion() {
        let mut limiter = TierRateLimiter::new();
        let balance = 100_000_000; // Storage tier = 5 per epoch
        let epoch = 10;

        for _ in 0..5 {
            assert!(limiter.check_rate_limit("alice", balance, epoch));
            limiter.record_submission("alice", epoch);
        }
        // 6th should be blocked
        assert!(!limiter.check_rate_limit("alice", balance, epoch));
    }

    #[test]
    fn test_epoch_reset() {
        let mut limiter = TierRateLimiter::new();
        let balance = 100_000_000;

        // Fill epoch 1
        for _ in 0..5 {
            limiter.record_submission("alice", 1);
        }
        assert!(!limiter.check_rate_limit("alice", balance, 1));

        // New epoch resets
        assert!(limiter.check_rate_limit("alice", balance, 2));
    }

    #[test]
    fn test_current_count() {
        let mut limiter = TierRateLimiter::new();
        limiter.record_submission("bob", 5);
        limiter.record_submission("bob", 5);
        assert_eq!(limiter.current_count("bob", 5), 2);
        assert_eq!(limiter.current_count("bob", 6), 0); // different epoch
    }
}
