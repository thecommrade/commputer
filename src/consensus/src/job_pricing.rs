/// Resource requirements for pricing calculation.
#[derive(Debug, Clone)]
pub struct ResourceReq {
    pub cpu_cores: u16,
    pub gpu_vram_mb: u64,
    pub ram_mb: u64,
    pub storage_mb: u64,
    pub bandwidth_mbps: u64,
}

/// Network pricing state.
#[derive(Debug, Clone)]
pub struct PricingState {
    /// Base rate per CPU-second in raw COMME units.
    pub base_rate_per_cpu_sec: u64,
    /// Number of currently active jobs on the network.
    pub active_jobs: u64,
    /// Total job capacity of the network.
    pub total_capacity: u64,
}

/// Minimum price floor in raw COMME units.
pub const MIN_PRICE_FLOOR: u64 = 1_000_000; // 0.01 COMME

/// Calculate the price for a compute job.
///
/// Formula: `base_rate * resource_units * duration * load_multiplier`
/// - resource_units = cpu_cores + gpu_units + ram_units + storage_units + bandwidth_units
/// - load_multiplier = max(1.0, (active/capacity)^2)
/// - Near capacity (>90%), price doubles for each additional 5% utilization.
pub fn calculate_job_price(
    state: &PricingState,
    resources: &ResourceReq,
    duration_secs: u64,
) -> u64 {
    if state.total_capacity == 0 || duration_secs == 0 {
        return MIN_PRICE_FLOOR;
    }

    // Calculate resource units (weighted sum)
    let resource_units = compute_resource_units(resources);

    // Base price
    let base_price = state.base_rate_per_cpu_sec as u128
        * resource_units as u128
        * duration_secs as u128;

    // Load multiplier (fixed-point with 1000x precision)
    let load_multiplier_1000 = compute_load_multiplier(state.active_jobs, state.total_capacity);

    // Apply load multiplier
    let adjusted = base_price * load_multiplier_1000 as u128 / 1000;

    // Apply surge pricing near capacity
    let surge = compute_surge_multiplier(state.active_jobs, state.total_capacity);
    let final_price = adjusted * surge as u128 / 1000;

    // Clamp to u64 and apply floor
    let price = if final_price > u64::MAX as u128 {
        u64::MAX
    } else {
        final_price as u64
    };

    price.max(MIN_PRICE_FLOOR)
}

/// Compute resource units as a weighted sum.
fn compute_resource_units(resources: &ResourceReq) -> u64 {
    let cpu = resources.cpu_cores as u64;
    let gpu = resources.gpu_vram_mb / 1024; // 1 unit per GB of VRAM
    let ram = resources.ram_mb / 4096; // 1 unit per 4 GB RAM
    let storage = resources.storage_mb / 102400; // 1 unit per 100 GB
    let bandwidth = resources.bandwidth_mbps / 1000; // 1 unit per Gbps

    // At least 1 unit
    (cpu + gpu + ram + storage + bandwidth).max(1)
}

/// Compute load multiplier (1000x fixed-point).
/// Formula: max(1.0, (active/capacity)^2) -> max(1000, (active*1000/capacity)^2 / 1000)
fn compute_load_multiplier(active: u64, capacity: u64) -> u64 {
    if capacity == 0 {
        return 1000;
    }
    let ratio_1000 = active * 1000 / capacity;
    let squared_1000 = ratio_1000 * ratio_1000 / 1000;
    squared_1000.max(1000)
}

/// Compute surge multiplier for near-capacity pricing (1000x fixed-point).
/// >90% utilization: doubles for each additional 5%.
fn compute_surge_multiplier(active: u64, capacity: u64) -> u64 {
    if capacity == 0 {
        return 1000;
    }
    let utilization_pct = active * 100 / capacity;

    if utilization_pct <= 90 {
        return 1000; // No surge
    }

    // Each 5% above 90% doubles the price
    let excess_pct = utilization_pct - 90;
    let doublings = excess_pct / 5;

    // 2^doublings * 1000
    1000u64.saturating_mul(1u64 << doublings)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_state() -> PricingState {
        PricingState {
            base_rate_per_cpu_sec: 100,
            active_jobs: 50,
            total_capacity: 100,
        }
    }

    fn basic_resources() -> ResourceReq {
        ResourceReq {
            cpu_cores: 4,
            gpu_vram_mb: 0,
            ram_mb: 8192,
            storage_mb: 0,
            bandwidth_mbps: 0,
        }
    }

    #[test]
    fn test_resource_units() {
        let r = ResourceReq {
            cpu_cores: 4,
            gpu_vram_mb: 8192,
            ram_mb: 16384,
            storage_mb: 102400,
            bandwidth_mbps: 1000,
        };
        // cpu=4, gpu=8, ram=4, storage=1, bandwidth=1 = 18
        assert_eq!(compute_resource_units(&r), 18);
    }

    #[test]
    fn test_resource_units_minimum() {
        let r = ResourceReq {
            cpu_cores: 0,
            gpu_vram_mb: 0,
            ram_mb: 0,
            storage_mb: 0,
            bandwidth_mbps: 0,
        };
        assert_eq!(compute_resource_units(&r), 1);
    }

    #[test]
    fn test_load_multiplier_low_load() {
        // 10% load: (100)^2 / 1000 = 10, max(1000, 10) = 1000
        assert_eq!(compute_load_multiplier(10, 100), 1000);
    }

    #[test]
    fn test_load_multiplier_high_load() {
        // 100% load: (1000)^2 / 1000 = 1000
        assert_eq!(compute_load_multiplier(100, 100), 1000);
    }

    #[test]
    fn test_load_multiplier_over_capacity() {
        // 150% load: (1500)^2 / 1000 = 2250
        assert_eq!(compute_load_multiplier(150, 100), 2250);
    }

    #[test]
    fn test_surge_no_surge() {
        assert_eq!(compute_surge_multiplier(50, 100), 1000);
        assert_eq!(compute_surge_multiplier(90, 100), 1000);
    }

    #[test]
    fn test_surge_at_95_pct() {
        // 95% = 5% excess = 1 doubling = 2x
        assert_eq!(compute_surge_multiplier(95, 100), 2000);
    }

    #[test]
    fn test_surge_at_100_pct() {
        // 100% = 10% excess = 2 doublings = 4x
        assert_eq!(compute_surge_multiplier(100, 100), 4000);
    }

    #[test]
    fn test_price_floor() {
        let state = PricingState {
            base_rate_per_cpu_sec: 1,
            active_jobs: 0,
            total_capacity: 100,
        };
        let resources = ResourceReq {
            cpu_cores: 1,
            gpu_vram_mb: 0,
            ram_mb: 0,
            storage_mb: 0,
            bandwidth_mbps: 0,
        };
        let price = calculate_job_price(&state, &resources, 1);
        assert_eq!(price, MIN_PRICE_FLOOR);
    }

    #[test]
    fn test_price_increases_with_load() {
        let resources = basic_resources();
        let low_load = PricingState {
            base_rate_per_cpu_sec: 1000,
            active_jobs: 10,
            total_capacity: 100,
        };
        let high_load = PricingState {
            base_rate_per_cpu_sec: 1000,
            active_jobs: 95,
            total_capacity: 100,
        };
        let price_low = calculate_job_price(&low_load, &resources, 3600);
        let price_high = calculate_job_price(&high_load, &resources, 3600);
        assert!(price_high > price_low, "high load price {} should be > low load price {}", price_high, price_low);
    }

    #[test]
    fn test_price_increases_with_duration() {
        let state = base_state();
        let resources = basic_resources();
        let price_short = calculate_job_price(&state, &resources, 60);
        let price_long = calculate_job_price(&state, &resources, 3600);
        assert!(price_long > price_short);
    }

    #[test]
    fn test_price_increases_with_resources() {
        let state = base_state();
        let small = ResourceReq {
            cpu_cores: 1,
            gpu_vram_mb: 0,
            ram_mb: 4096,
            storage_mb: 0,
            bandwidth_mbps: 0,
        };
        let large = ResourceReq {
            cpu_cores: 16,
            gpu_vram_mb: 16384,
            ram_mb: 65536,
            storage_mb: 1024000,
            bandwidth_mbps: 10000,
        };
        let price_small = calculate_job_price(&state, &small, 3600);
        let price_large = calculate_job_price(&state, &large, 3600);
        assert!(price_large > price_small);
    }

    #[test]
    fn test_zero_capacity() {
        let state = PricingState {
            base_rate_per_cpu_sec: 1000,
            active_jobs: 10,
            total_capacity: 0,
        };
        let price = calculate_job_price(&state, &basic_resources(), 3600);
        assert_eq!(price, MIN_PRICE_FLOOR);
    }

    #[test]
    fn test_zero_duration() {
        let price = calculate_job_price(&base_state(), &basic_resources(), 0);
        assert_eq!(price, MIN_PRICE_FLOOR);
    }

    #[test]
    fn test_surge_pricing_dramatic_increase() {
        let resources = basic_resources();
        // At 90% - no surge
        let state_90 = PricingState {
            base_rate_per_cpu_sec: 10000,
            active_jobs: 90,
            total_capacity: 100,
        };
        // At 100% - 4x surge
        let state_100 = PricingState {
            base_rate_per_cpu_sec: 10000,
            active_jobs: 100,
            total_capacity: 100,
        };
        let price_90 = calculate_job_price(&state_90, &resources, 3600);
        let price_100 = calculate_job_price(&state_100, &resources, 3600);
        // Should be roughly 4x more expensive at 100% vs 90%
        assert!(price_100 > price_90 * 2, "Expected significant surge pricing");
    }
}
