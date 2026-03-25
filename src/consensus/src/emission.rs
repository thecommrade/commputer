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

    // Feature 200: Emission curve integration test — verify 2B cap never exceeded
    #[test]
    fn feature_200_emission_curve_2b_cap() {
        let schedule = EmissionSchedule::new();
        let mut total_emitted: u64 = 0;
        let total_supply = commputer_core::token::TOTAL_SUPPLY;

        // Simulate many epochs at various validator counts
        // 24 epochs per day, 365 days per year, 10 years
        let validator_counts = [100, 1_000, 10_000, 50_000, 100_000, 500_000, 1_000_000];
        for &count in &validator_counts {
            // Simulate 1 year at this validator count (24 * 365 = 8760 epochs)
            for _ in 0..8760 {
                let epoch_emission = schedule.per_epoch_emission(count);
                total_emitted = total_emitted.saturating_add(epoch_emission);

                // Never exceed total supply
                assert!(
                    total_emitted <= total_supply,
                    "Emission exceeded 2B cap at {} validators: {} > {}",
                    count,
                    total_emitted,
                    total_supply
                );
            }
        }

        // Verify floor rate is respected even at massive scale
        let floor_daily = UNITS_PER_COMME / 100; // 0.01 COMME/day
        let rate_at_billion = schedule.per_validator_daily_rate(1_000_000_000);
        assert!(
            rate_at_billion >= floor_daily,
            "Floor rate not respected at 1B validators: {} < {}",
            rate_at_billion,
            floor_daily
        );
    }

    // Feature 217: Gold standard test — verify reference node yields ~33 COMME/year
    #[test]
    fn feature_217_gold_standard_reference_node() {
        let schedule = EmissionSchedule::new();
        // At launch (few validators, below curve start), daily rate = 0.09 COMME
        let daily = schedule.per_validator_daily_rate(1);
        let yearly = daily * 365;
        let yearly_comme = yearly / UNITS_PER_COMME;

        // Gold standard: 0.3225 troy oz / 10.03g of gold in 2026
        // Base emission: 0.09 COMME/day -> ~32.85 COMME/year
        assert!(
            yearly_comme >= 32 && yearly_comme <= 34,
            "Expected ~33 COMME/year at launch, got {}",
            yearly_comme
        );

        // Verify the exact daily rate in raw units
        let expected_daily = (UNITS_PER_COMME * 9) / 100; // 0.09 COMME = 9_000_000 units
        assert_eq!(daily, expected_daily, "Daily rate should be exactly 0.09 COMME");
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
