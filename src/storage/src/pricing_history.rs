use serde::{Deserialize, Serialize};

/// A single price data point at a specific height/epoch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricePoint {
    pub height: u64,
    pub epoch: u64,
    pub cpu_price: u64,
    pub gpu_price: u64,
    pub storage_price: u64,
    pub ram_price: u64,
    pub utilization_pct: f64,
}

/// Resource type for price queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceType {
    Cpu,
    Gpu,
    Storage,
    Ram,
}

/// Historical pricing data for compute resources.
pub struct PricingHistory {
    pub points: Vec<PricePoint>,
    pub max_points: usize,
}

impl PricingHistory {
    pub fn new(max_points: usize) -> Self {
        Self {
            points: Vec::new(),
            max_points,
        }
    }

    /// Record a new price point.
    pub fn record_price(&mut self, point: PricePoint) {
        if self.points.len() >= self.max_points {
            self.points.remove(0);
        }
        self.points.push(point);
    }

    /// Get the last N price points.
    pub fn get_history(&self, last_n: usize) -> &[PricePoint] {
        let len = self.points.len();
        if last_n >= len {
            &self.points
        } else {
            &self.points[len - last_n..]
        }
    }

    /// Calculate average price for a given resource type across all recorded points.
    pub fn average_price(&self, resource_type: ResourceType) -> f64 {
        if self.points.is_empty() {
            return 0.0;
        }
        let sum: u64 = self
            .points
            .iter()
            .map(|p| match resource_type {
                ResourceType::Cpu => p.cpu_price,
                ResourceType::Gpu => p.gpu_price,
                ResourceType::Storage => p.storage_price,
                ResourceType::Ram => p.ram_price,
            })
            .sum();
        sum as f64 / self.points.len() as f64
    }

    /// Number of recorded price points.
    pub fn len(&self) -> usize {
        self.points.len()
    }

    /// Whether the history is empty.
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_point(height: u64, cpu: u64, gpu: u64) -> PricePoint {
        PricePoint {
            height,
            epoch: height / 100,
            cpu_price: cpu,
            gpu_price: gpu,
            storage_price: 10,
            ram_price: 20,
            utilization_pct: 50.0,
        }
    }

    #[test]
    fn test_record_and_get() {
        let mut hist = PricingHistory::new(100);
        hist.record_price(make_point(100, 1000, 5000));
        hist.record_price(make_point(200, 1100, 5500));
        hist.record_price(make_point(300, 1200, 6000));

        assert_eq!(hist.len(), 3);
        let last2 = hist.get_history(2);
        assert_eq!(last2.len(), 2);
        assert_eq!(last2[0].height, 200);
        assert_eq!(last2[1].height, 300);
    }

    #[test]
    fn test_max_points_eviction() {
        let mut hist = PricingHistory::new(3);
        for i in 0..5 {
            hist.record_price(make_point(i * 100, 1000, 5000));
        }
        assert_eq!(hist.len(), 3);
        assert_eq!(hist.points[0].height, 200);
    }

    #[test]
    fn test_average_price() {
        let mut hist = PricingHistory::new(100);
        hist.record_price(make_point(100, 1000, 4000));
        hist.record_price(make_point(200, 2000, 6000));

        let avg_cpu = hist.average_price(ResourceType::Cpu);
        assert!((avg_cpu - 1500.0).abs() < 0.01);

        let avg_gpu = hist.average_price(ResourceType::Gpu);
        assert!((avg_gpu - 5000.0).abs() < 0.01);
    }

    #[test]
    fn test_empty_average() {
        let hist = PricingHistory::new(10);
        assert_eq!(hist.average_price(ResourceType::Cpu), 0.0);
    }

    #[test]
    fn test_get_history_more_than_available() {
        let mut hist = PricingHistory::new(100);
        hist.record_price(make_point(100, 1000, 5000));
        let all = hist.get_history(50);
        assert_eq!(all.len(), 1);
    }
}
