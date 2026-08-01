use commputer_core::proof::ResourceChannel;
use commputer_core::token::UNITS_PER_COMME;
use std::collections::HashMap;

// Re-export halving constants from commputer-core (single source of truth).
pub use commputer_core::token::{INITIAL_BLOCK_REWARD, HALVING_INTERVAL, MAX_HALVINGS};

/// The emission schedule.
pub struct EmissionSchedule {
    _private: (),
}

impl EmissionSchedule {
    pub fn new() -> Self {
        Self { _private: () }
    }

    /// Block reward at a given block height.
    /// Delegates to commputer_core::token::block_reward (single source of truth).
    pub fn block_reward(&self, height: u64) -> u64 {
        commputer_core::token::block_reward(height)
    }

    /// Calculate per-validator daily emission rate given current block height
    /// and validator count. This is for display/estimation purposes.
    pub fn per_validator_daily_rate(&self, height: u64, validator_count: u64) -> u64 {
        if validator_count == 0 {
            return 0;
        }
        let reward = self.block_reward(height);
        let blocks_per_day: u64 = 43_200; // 2-second blocks
        reward * blocks_per_day / validator_count
    }

    /// Total daily network emission at a given block height.
    pub fn total_daily_emission(&self, height: u64) -> u64 {
        let reward = self.block_reward(height);
        reward * 43_200
    }

    /// Per-epoch emission (epochs are 1 hour = 1/24 of a day).
    pub fn per_epoch_emission(&self, height: u64, validator_count: u64) -> u64 {
        if validator_count == 0 {
            return 0;
        }
        self.total_daily_emission(height) / 24
    }

    /// Human-readable description of current emission rate.
    pub fn describe(&self, height: u64, validator_count: u64) -> String {
        let era = height / HALVING_INTERVAL;
        let reward_comme = self.block_reward(height) as f64 / UNITS_PER_COMME as f64;
        let daily_per_node = self.per_validator_daily_rate(height, validator_count) as f64 / UNITS_PER_COMME as f64;
        let yearly_per_node = daily_per_node * 365.0;
        format!(
            "Era {} (halving {}): {:.4} COMME/block, {:.2} COMME/day/node, {:.0} COMME/year/node ({} validators)",
            era, era, reward_comme, daily_per_node, yearly_per_node, validator_count
        )
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
    fn initial_block_reward() {
        let schedule = EmissionSchedule::new();
        assert_eq!(schedule.block_reward(0), INITIAL_BLOCK_REWARD);
        assert_eq!(schedule.block_reward(1), INITIAL_BLOCK_REWARD);
        assert_eq!(schedule.block_reward(HALVING_INTERVAL - 1), INITIAL_BLOCK_REWARD);
    }

    #[test]
    fn first_halving() {
        let schedule = EmissionSchedule::new();
        let era0 = schedule.block_reward(0);
        let era1 = schedule.block_reward(HALVING_INTERVAL);
        assert_eq!(era1, era0 / 2);
    }

    #[test]
    fn second_halving() {
        let schedule = EmissionSchedule::new();
        let era0 = schedule.block_reward(0);
        let era2 = schedule.block_reward(HALVING_INTERVAL * 2);
        assert_eq!(era2, era0 / 4);
    }

    #[test]
    fn reward_reaches_zero() {
        let schedule = EmissionSchedule::new();
        let reward = schedule.block_reward(HALVING_INTERVAL * 33);
        assert_eq!(reward, 0);
    }

    #[test]
    fn fifty_percent_in_four_years() {
        // No schedule instance needed: this asserts on the CONSTANTS alone.
        // Era 0: 63,072,000 blocks x 15,854,895 units/block
        let era0_total = HALVING_INTERVAL as u128 * INITIAL_BLOCK_REWARD as u128;
        let total_supply = 2_000_000_000u128 * UNITS_PER_COMME as u128;
        let percent = (era0_total * 100) / total_supply;
        // Should be approximately 50%
        assert!(percent >= 49 && percent <= 51,
            "Era 0 should emit ~50% of supply, got {}%", percent);
    }

    #[test]
    fn per_validator_daily_at_25_nodes() {
        let schedule = EmissionSchedule::new();
        let daily = schedule.per_validator_daily_rate(0, 25);
        let daily_comme = daily as f64 / UNITS_PER_COMME as f64;
        // 15.85 COMME/block x 43200 blocks/day / 25 nodes = ~27,405 COMME/day
        assert!(daily_comme > 27_000.0 && daily_comme < 28_000.0,
            "Expected ~27,405 COMME/day at 25 nodes, got {:.0}", daily_comme);
    }

    #[test]
    fn per_validator_daily_at_10k_nodes() {
        let schedule = EmissionSchedule::new();
        let daily = schedule.per_validator_daily_rate(0, 10_000);
        let daily_comme = daily as f64 / UNITS_PER_COMME as f64;
        // 15.85 x 43200 / 10000 = ~68.5 COMME/day
        assert!(daily_comme > 60.0 && daily_comme < 75.0,
            "Expected ~68.5 COMME/day at 10K nodes, got {:.1}", daily_comme);
    }

    #[test]
    fn total_emission_never_exceeds_supply() {
        let schedule = EmissionSchedule::new();
        let mut total: u128 = 0;
        let total_supply = 2_000_000_000u128 * UNITS_PER_COMME as u128;

        // Simulate all eras
        for era in 0..=MAX_HALVINGS {
            let reward = schedule.block_reward(era as u64 * HALVING_INTERVAL) as u128;
            let era_total = reward * HALVING_INTERVAL as u128;
            total += era_total;
        }

        assert!(total <= total_supply,
            "Total emission {} exceeds supply {}", total, total_supply);
    }

    #[test]
    fn channel_floors_respected() {
        let total = 1_000_000u64;
        let demand = HashMap::new();
        let alloc = ChannelAllocation::from_demand(total, &demand);

        for channel in ResourceChannel::ALL {
            let floor = total * channel.emission_floor_bps() as u64 / 10000;
            assert!(alloc.get(&channel) >= floor);
        }
    }

    #[test]
    fn demand_weighted_surplus() {
        let total = 1_000_000u64;
        let mut demand = HashMap::new();
        demand.insert(ResourceChannel::Gpu, 1000);
        demand.insert(ResourceChannel::Processing, 100);
        demand.insert(ResourceChannel::Storage, 100);
        demand.insert(ResourceChannel::Ram, 100);
        demand.insert(ResourceChannel::Bandwidth, 100);

        let alloc = ChannelAllocation::from_demand(total, &demand);
        assert!(alloc.get(&ResourceChannel::Gpu) > alloc.get(&ResourceChannel::Processing));
    }
}
