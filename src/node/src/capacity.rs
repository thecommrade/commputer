use serde::{Deserialize, Serialize};

/// Network-wide capacity report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapacityReport {
    pub total_cpu_cores: u64,
    pub total_gpu_vram_mb: u64,
    pub total_ram_mb: u64,
    pub total_storage_mb: u64,
    pub used_cpu_cores: u64,
    pub used_gpu_vram_mb: u64,
    pub used_ram_mb: u64,
    pub available_cpu_cores: u64,
    pub available_gpu_vram_mb: u64,
    pub available_ram_mb: u64,
    pub utilization_percent: f64,
    pub current_price_per_cpu_hour: u64,
    pub active_validators: u64,
    pub active_jobs: u64,
    pub pending_jobs: u64,
    pub flagship_capacity_used_percent: f64,
    pub other_capacity_used_percent: f64,
}

impl CapacityReport {
    /// Create a sample/mock capacity report.
    pub fn mock() -> Self {
        Self {
            total_cpu_cores: 10000,
            total_gpu_vram_mb: 500_000,
            total_ram_mb: 5_000_000,
            total_storage_mb: 50_000_000,
            used_cpu_cores: 3000,
            used_gpu_vram_mb: 100_000,
            used_ram_mb: 1_500_000,
            available_cpu_cores: 7000,
            available_gpu_vram_mb: 400_000,
            available_ram_mb: 3_500_000,
            utilization_percent: 30.0,
            current_price_per_cpu_hour: 500_000, // 0.005 COMME/cpu-hour
            active_validators: 500,
            active_jobs: 150,
            pending_jobs: 25,
            flagship_capacity_used_percent: 40.0,
            other_capacity_used_percent: 20.0,
        }
    }

    /// Calculate utilization from used/total.
    pub fn calculate_utilization(used_cpu: u64, total_cpu: u64) -> f64 {
        if total_cpu == 0 {
            return 0.0;
        }
        (used_cpu as f64 / total_cpu as f64) * 100.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_utilization() {
        assert_eq!(CapacityReport::calculate_utilization(50, 100), 50.0);
        assert_eq!(CapacityReport::calculate_utilization(0, 100), 0.0);
        assert_eq!(CapacityReport::calculate_utilization(0, 0), 0.0);
    }

    #[test]
    fn test_mock_report() {
        let report = CapacityReport::mock();
        assert!(report.total_cpu_cores > 0);
        assert!(report.available_cpu_cores <= report.total_cpu_cores);
    }
}
