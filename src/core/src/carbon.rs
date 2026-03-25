//! Carbon footprint estimation (#50)

#[derive(Debug, Clone, PartialEq)]
pub struct CarbonEstimate { pub watts_per_validator: f64, pub total_validators: u64, pub total_kwh_per_year: f64, pub co2_kg_per_year: f64 }

const WATTS_PER_VALIDATOR: f64 = 150.0;
const HOURS_PER_YEAR: f64 = 8760.0;
const CO2_KG_PER_KWH: f64 = 0.4;
pub const BITCOIN_ESTIMATED_TWH_PER_YEAR: f64 = 150.0;

pub fn estimate_carbon(validator_count: u64) -> CarbonEstimate {
    let total_kw = WATTS_PER_VALIDATOR * validator_count as f64 / 1000.0;
    let total_kwh_per_year = total_kw * HOURS_PER_YEAR;
    CarbonEstimate { watts_per_validator: WATTS_PER_VALIDATOR, total_validators: validator_count, total_kwh_per_year, co2_kg_per_year: total_kwh_per_year * CO2_KG_PER_KWH }
}

pub fn bitcoin_comparison_ratio(estimate: &CarbonEstimate) -> f64 { estimate.total_kwh_per_year / 1_000_000_000.0 / BITCOIN_ESTIMATED_TWH_PER_YEAR }

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn test_single() { let e = estimate_carbon(1); assert!((e.total_kwh_per_year - 1314.0).abs() < 0.1); assert!((e.co2_kg_per_year - 525.6).abs() < 0.1); }
    #[test] fn test_zero() { let e = estimate_carbon(0); assert!(e.total_kwh_per_year.abs() < f64::EPSILON); }
    #[test] fn test_1k() { let e = estimate_carbon(1_000); assert!((e.total_kwh_per_year - 1_314_000.0).abs() < 1.0); }
    #[test] fn test_vs_bitcoin() { let e = estimate_carbon(1_000_000); assert!(bitcoin_comparison_ratio(&e) < 0.01); }
    #[test] fn test_linear() { let a = estimate_carbon(100); let b = estimate_carbon(200); assert!((b.total_kwh_per_year / a.total_kwh_per_year - 2.0).abs() < f64::EPSILON); }
}
