//! Dynamic burst compute pricing (#48)

#[derive(Debug, Clone)]
pub struct BurstPriceCalculator { pub base_price: u64, pub demand_factor: f64, pub supply_factor: f64, pub min_price: u64 }

impl BurstPriceCalculator {
    pub fn new(base_price: u64) -> Self { Self { base_price, demand_factor: 2.0, supply_factor: 1.0, min_price: base_price / 2 } }
    pub fn calculate_price(&self, demand: u64, supply: u64) -> u64 {
        if supply == 0 { return u64::MAX; }
        let price = if demand > supply {
            let ratio = demand as f64 / supply as f64;
            let raw = self.base_price as f64 * (1.0 + self.demand_factor * (ratio - 1.0)) * self.supply_factor;
            if raw > u64::MAX as f64 { u64::MAX } else { raw as u64 }
        } else { self.base_price };
        price.max(self.min_price)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn test_equal() { assert_eq!(BurstPriceCalculator::new(1000).calculate_price(100, 100), 1000); }
    #[test] fn test_low() { assert_eq!(BurstPriceCalculator::new(1000).calculate_price(50, 100), 1000); }
    #[test] fn test_double() { assert_eq!(BurstPriceCalculator::new(1000).calculate_price(200, 100), 3000); }
    #[test] fn test_triple() { assert_eq!(BurstPriceCalculator::new(1000).calculate_price(300, 100), 5000); }
    #[test] fn test_zero_supply() { assert_eq!(BurstPriceCalculator::new(1000).calculate_price(100, 0), u64::MAX); }
    #[test] fn test_floor() { let mut c = BurstPriceCalculator::new(100); c.min_price = 500; assert_eq!(c.calculate_price(50, 100), 500); }
    #[test] fn test_custom_factor() { let mut c = BurstPriceCalculator::new(1000); c.demand_factor = 5.0; assert_eq!(c.calculate_price(200, 100), 6000); }
}
