//! Item 147: Proof challenge scheduling fairness.
//!
//! Ensures each validator gets challenged proportionally to their
//! claimed resources. Validators claiming more resources get challenged
//! more often, preventing free-riding.

use commputer_core::identity::Address;
use commputer_core::proof::ResourceChannel;

use std::collections::HashMap;

/// A validator's claimed resource capacity.
#[derive(Debug, Clone)]
pub struct ResourceClaim {
    pub validator: Address,
    /// Per-channel claimed capacity (0-100 scale).
    pub channel_claims: HashMap<ResourceChannel, u32>,
}

/// Fair challenge scheduler that distributes challenges proportionally.
pub struct FairScheduler {
    /// All registered validators and their claims.
    claims: Vec<ResourceClaim>,
    /// History of challenges issued per validator per channel.
    challenge_counts: HashMap<(Address, ResourceChannel), u64>,
}

/// A scheduled challenge assignment.
#[derive(Debug, Clone)]
pub struct ScheduledChallenge {
    pub validator: Address,
    pub channel: ResourceChannel,
    /// Priority weight (higher = should be challenged sooner).
    pub priority: f64,
}

impl FairScheduler {
    /// Create a new fair scheduler.
    pub fn new() -> Self {
        Self {
            claims: Vec::new(),
            challenge_counts: HashMap::new(),
        }
    }

    /// Register or update a validator's resource claims.
    pub fn register_claim(&mut self, claim: ResourceClaim) {
        // Replace existing claim for this validator.
        self.claims.retain(|c| c.validator != claim.validator);
        self.claims.push(claim);
    }

    /// Remove a validator from the scheduler.
    pub fn remove_validator(&mut self, validator: &Address) {
        self.claims.retain(|c| c.validator != *validator);
    }

    /// Record that a challenge was issued.
    pub fn record_challenge(&mut self, validator: Address, channel: ResourceChannel) {
        *self.challenge_counts.entry((validator, channel)).or_insert(0) += 1;
    }

    /// Generate fair challenge assignments for this epoch.
    ///
    /// Validators claiming higher capacity get challenged more often.
    /// Also accounts for under-challenged validators (catch-up).
    pub fn schedule_challenges(
        &self,
        epoch: u64,
        max_challenges_per_epoch: usize,
    ) -> Vec<ScheduledChallenge> {
        if self.claims.is_empty() {
            return vec![];
        }

        let mut scheduled = Vec::new();

        for claim in &self.claims {
            for channel in ResourceChannel::ALL {
                let claimed = claim.channel_claims.get(&channel).copied().unwrap_or(0);
                if claimed == 0 {
                    continue;
                }

                let past_count = self.challenge_counts
                    .get(&(claim.validator, channel))
                    .copied()
                    .unwrap_or(0);

                // Priority = claimed_capacity * catch_up_factor.
                // Validators who have been under-challenged get boosted.
                let expected = claimed as f64 * (epoch + 1) as f64 / 100.0;
                let catch_up = if past_count == 0 {
                    2.0
                } else {
                    (expected / past_count as f64).clamp(0.5, 3.0)
                };

                let priority = claimed as f64 * catch_up;

                scheduled.push(ScheduledChallenge {
                    validator: claim.validator,
                    channel,
                    priority,
                });
            }
        }

        // Sort by priority descending, take top N.
        scheduled.sort_by(|a, b| b.priority.partial_cmp(&a.priority).unwrap_or(std::cmp::Ordering::Equal));
        scheduled.truncate(max_challenges_per_epoch);
        scheduled
    }

    /// Check fairness: compute the coefficient of variation of challenge rates.
    /// Lower = more fair. Returns None if fewer than 2 validators.
    pub fn fairness_metric(&self) -> Option<f64> {
        if self.claims.len() < 2 {
            return None;
        }

        let rates: Vec<f64> = self.claims.iter().map(|claim| {
            let total_claims: u32 = claim.channel_claims.values().sum();
            let total_challenges: u64 = ResourceChannel::ALL.iter()
                .map(|ch| self.challenge_counts.get(&(claim.validator, *ch)).copied().unwrap_or(0))
                .sum();

            if total_claims == 0 {
                0.0
            } else {
                total_challenges as f64 / total_claims as f64
            }
        }).collect();

        let mean = rates.iter().sum::<f64>() / rates.len() as f64;
        if mean == 0.0 {
            return Some(0.0);
        }

        let variance = rates.iter()
            .map(|r| (r - mean).powi(2))
            .sum::<f64>() / rates.len() as f64;
        let cv = variance.sqrt() / mean;
        Some(cv)
    }

    /// Get the number of registered validators.
    pub fn validator_count(&self) -> usize {
        self.claims.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_addr(n: u8) -> Address {
        let mut a = [0u8; 32];
        a[0] = n;
        Address(a)
    }

    fn make_claim(n: u8, cpu: u32, gpu: u32) -> ResourceClaim {
        let mut claims = HashMap::new();
        claims.insert(ResourceChannel::Processing, cpu);
        claims.insert(ResourceChannel::Gpu, gpu);
        ResourceClaim {
            validator: test_addr(n),
            channel_claims: claims,
        }
    }

    #[test]
    fn item_147_schedule_proportional() {
        let mut scheduler = FairScheduler::new();
        scheduler.register_claim(make_claim(1, 100, 50)); // High capacity
        scheduler.register_claim(make_claim(2, 10, 10));  // Low capacity

        let scheduled = scheduler.schedule_challenges(0, 100);
        assert!(!scheduled.is_empty());

        // High-capacity validator should have higher total priority.
        let v1_priority: f64 = scheduled.iter()
            .filter(|s| s.validator == test_addr(1))
            .map(|s| s.priority)
            .sum();
        let v2_priority: f64 = scheduled.iter()
            .filter(|s| s.validator == test_addr(2))
            .map(|s| s.priority)
            .sum();
        assert!(v1_priority > v2_priority);
    }

    #[test]
    fn item_147_catch_up_boost() {
        let mut scheduler = FairScheduler::new();
        scheduler.register_claim(make_claim(1, 50, 50));
        scheduler.register_claim(make_claim(2, 50, 50));

        // Validator 1 has been challenged a lot, validator 2 has not.
        for _ in 0..20 {
            scheduler.record_challenge(test_addr(1), ResourceChannel::Processing);
        }

        let scheduled = scheduler.schedule_challenges(5, 10);
        // Validator 2 should have higher priority (catch-up).
        let v2_top = scheduled.iter()
            .find(|s| s.validator == test_addr(2) && s.channel == ResourceChannel::Processing);
        let v1_top = scheduled.iter()
            .find(|s| s.validator == test_addr(1) && s.channel == ResourceChannel::Processing);

        if let (Some(v2), Some(v1)) = (v2_top, v1_top) {
            assert!(v2.priority > v1.priority, "under-challenged validator should get priority boost");
        }
    }

    #[test]
    fn item_147_fairness_metric() {
        let mut scheduler = FairScheduler::new();
        scheduler.register_claim(make_claim(1, 50, 50));
        scheduler.register_claim(make_claim(2, 50, 50));

        // Equal challenges -> low CV.
        scheduler.record_challenge(test_addr(1), ResourceChannel::Processing);
        scheduler.record_challenge(test_addr(2), ResourceChannel::Processing);

        let cv = scheduler.fairness_metric().unwrap();
        assert!(cv < 1.0, "equal challenges should have low CV: {}", cv);
    }

    #[test]
    fn item_147_empty_scheduler() {
        let scheduler = FairScheduler::new();
        let scheduled = scheduler.schedule_challenges(0, 10);
        assert!(scheduled.is_empty());
        assert!(scheduler.fairness_metric().is_none());
    }

    #[test]
    fn item_147_remove_validator() {
        let mut scheduler = FairScheduler::new();
        scheduler.register_claim(make_claim(1, 50, 50));
        scheduler.register_claim(make_claim(2, 50, 50));
        assert_eq!(scheduler.validator_count(), 2);

        scheduler.remove_validator(&test_addr(1));
        assert_eq!(scheduler.validator_count(), 1);
    }

    #[test]
    fn item_147_max_challenges_limit() {
        let mut scheduler = FairScheduler::new();
        for i in 0..10 {
            scheduler.register_claim(make_claim(i, 50, 50));
        }
        let scheduled = scheduler.schedule_challenges(0, 5);
        assert!(scheduled.len() <= 5);
    }
}
