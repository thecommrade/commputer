use commputer_core::proof::ResourceChannel;
use commputer_core::token::UNITS_PER_COMME;
use std::collections::HashMap;

/// The hybrid emission curve.
///
/// Design principles:
/// - A single maxed desktop earns ~0.09 COMME/day at launch
/// - Rate adjusts downward as network grows (hybrid curve)
/// - Floor rate ensures mining always produces something
/// - Demand weighting distributes across 5 channels based on network need
/// - Minimum floors per channel prevent any channel from going to zero
pub struct EmissionSchedule {
    /// Base rate per validator per day in raw units (at launch network size).
    base_rate_per_day: u64,
    /// Floor rate — minimum a validator can earn per day regardless of network size.
    floor_rate_per_day: u64,
    /// Network size at which the curve begins reducing per-node rate.
    curve_start_validators: u64,
}

impl EmissionSchedule {
    pub fn new() -> Self {
        Self {
            // 0.09 COMME/day = 9_000_000 raw units/day
            base_rate_per_day: (UNITS_PER_COMME * 9) / 100,
            // 0.01 COMME/day = 1_000_000 raw units/day (floor)
            floor_rate_per_day: UNITS_PER_COMME / 100,
            // Curve kicks in after 10,000 validators
            curve_start_validators: 10_000,
        }
    }

    /// Calculate per-validator daily emission rate given current network size.
    /// Uses inverse square root scaling above the curve start threshold.
    pub fn per_validator_daily_rate(&self, validator_count: u64) -> u64 {
        if validator_count == 0 {
            return 0;
        }

        if validator_count <= self.curve_start_validators {
            return self.base_rate_per_day;
        }

        // Inverse sqrt scaling: rate = base * sqrt(curve_start / validator_count)
        // This gives a gentle decline that stretches supply across decades.
        let ratio = self.curve_start_validators as f64 / validator_count as f64;
        let scaled = (self.base_rate_per_day as f64 * ratio.sqrt()) as u64;

        // Never go below the floor.
        scaled.max(self.floor_rate_per_day)
    }

    /// Total daily network emission for a given validator count.
    pub fn total_daily_emission(&self, validator_count: u64) -> u64 {
        self.per_validator_daily_rate(validator_count) * validator_count
    }

    /// Per-epoch emission (epochs are 1 hour = 1/24 of a day).
    pub fn per_epoch_emission(&self, validator_count: u64) -> u64 {
        self.total_daily_emission(validator_count) / 24
    }
}

impl Default for EmissionSchedule {
    fn default() -> Self {
        Self::new()
    }
}

/// Allocation of epoch emission across the 5 resource channels.
#[derive(Debug, Clone)]
pub struct ChannelAllocation {
    /// Emission per channel for this epoch.
    pub allocation: HashMap<ResourceChannel, u64>,
}

impl ChannelAllocation {
    /// Compute demand-weighted allocation with guaranteed floors.
    ///
    /// 1. Each channel gets its floor (10% or 5%).
    /// 2. The remaining 60% is distributed proportional to demand.
    /// 3. If demand is zero across all channels, surplus is split equally.
    pub fn from_demand(
        total_emission: u64,
        demand: &HashMap<ResourceChannel, u64>,
    ) -> Self {
        let mut allocation = HashMap::new();

        // Step 1: Allocate floors.
        let mut floor_total = 0u64;
        for channel in ResourceChannel::ALL {
            let floor = total_emission * channel.emission_floor_bps() as u64 / 10000;
            allocation.insert(channel, floor);
            floor_total += floor;
        }

        // Step 2: Distribute surplus based on demand.
        let surplus = total_emission.saturating_sub(floor_total);
        if surplus > 0 {
            let total_demand: u64 = demand.values().sum();
            if total_demand > 0 {
                for channel in ResourceChannel::ALL {
                    let channel_demand = demand.get(&channel).copied().unwrap_or(0);
                    let share = surplus * channel_demand / total_demand;
                    *allocation.entry(channel).or_insert(0) += share;
                }
            } else {
                // No demand signal — split surplus equally.
                let equal_share = surplus / 5;
                for channel in ResourceChannel::ALL {
                    *allocation.entry(channel).or_insert(0) += equal_share;
                }
            }
        }

        Self { allocation }
    }

    /// Get the emission for a specific channel.
    pub fn get(&self, channel: &ResourceChannel) -> u64 {
        self.allocation.get(channel).copied().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_rate_at_launch() {
        let schedule = EmissionSchedule::new();
        let rate = schedule.per_validator_daily_rate(1000);
        // Below curve start (10k), should be base rate.
        assert_eq!(rate, (UNITS_PER_COMME * 9) / 100);
    }

    #[test]
    fn rate_decreases_with_scale() {
        let schedule = EmissionSchedule::new();
        let rate_10k = schedule.per_validator_daily_rate(10_000);
        let rate_100k = schedule.per_validator_daily_rate(100_000);
        let rate_1m = schedule.per_validator_daily_rate(1_000_000);

        assert!(rate_10k > rate_100k);
        assert!(rate_100k > rate_1m);
    }

    #[test]
    fn floor_rate_enforced() {
        let schedule = EmissionSchedule::new();
        let rate = schedule.per_validator_daily_rate(100_000_000); // 100M validators
        assert!(rate >= UNITS_PER_COMME / 100); // Floor: 0.01 COMME/day
    }

    #[test]
    fn one_year_to_33_at_launch() {
        let schedule = EmissionSchedule::new();
        let daily = schedule.per_validator_daily_rate(5000);
        let yearly = daily * 365;
        let whole_comme = yearly / UNITS_PER_COMME;
        // Should be roughly 33 COMME in a year.
        assert!(whole_comme >= 30 && whole_comme <= 36,
            "Expected ~33 COMME/year, got {}", whole_comme);
    }

    #[test]
    fn channel_floors_respected() {
        let total = 1_000_000u64;
        let demand = HashMap::new(); // No demand signal.
        let alloc = ChannelAllocation::from_demand(total, &demand);

        // Each channel should get at least its floor.
        for channel in ResourceChannel::ALL {
            let floor = total * channel.emission_floor_bps() as u64 / 10000;
            assert!(alloc.get(&channel) >= floor);
        }
    }

    #[test]
    fn nerfed_validator_earns_20_percent() {
        use commputer_core::compliance::NerfRate;
        let schedule = EmissionSchedule::new();
        let full_rate = schedule.per_validator_daily_rate(1000);
        let nerf = NerfRate::INITIAL; // 80% nerf
        let nerfed_rate = (full_rate as f64 * nerf.reward_multiplier()).round() as u64;
        // 80% nerf means 20% reward
        assert!(nerfed_rate > 0);
        assert!(nerfed_rate < full_rate);
        assert_eq!(nerfed_rate, full_rate / 5);
    }

    #[test]
    fn epoch_reward_distribution() {
        let schedule = EmissionSchedule::new();
        let validator_count = 10;
        let epoch_emission = schedule.per_epoch_emission(validator_count);
        let per_validator = epoch_emission / validator_count;
        assert!(per_validator > 0);
        // Epoch is 1/24 of a day
        let daily = schedule.per_validator_daily_rate(validator_count);
        let expected_epoch = daily / 24;
        assert_eq!(per_validator, expected_epoch);
    }

    #[test]
    fn demand_weighted_surplus() {
        let total = 1_000_000u64;
        let mut demand = HashMap::new();
        // GPU demand is 10x everything else.
        demand.insert(ResourceChannel::Gpu, 1000);
        demand.insert(ResourceChannel::Processing, 100);
        demand.insert(ResourceChannel::Storage, 100);
        demand.insert(ResourceChannel::Ram, 100);
        demand.insert(ResourceChannel::Bandwidth, 100);

        let alloc = ChannelAllocation::from_demand(total, &demand);
        // GPU should get significantly more than other channels.
        assert!(alloc.get(&ResourceChannel::Gpu) > alloc.get(&ResourceChannel::Processing));
    }
}
