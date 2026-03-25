//! Network health score (#49)

#[derive(Debug, Clone, PartialEq)]
pub struct NetworkHealthScore { pub validator_score: f64, pub economic_score: f64, pub activity_score: f64, pub composite: f64 }

pub fn calculate_health(validator_growth: f64, burn_rate: f64, tx_volume: u64, new_accounts: u64) -> NetworkHealthScore {
    let vs = if validator_growth < -10.0 { 0.0 } else if validator_growth < 0.0 { 40.0 + validator_growth * 4.0 } else if validator_growth <= 10.0 { 40.0 + validator_growth * 6.0 } else { 100.0 };
    let es = if burn_rate < 0.0 { 0.0 } else if burn_rate <= 0.5 { 50.0 + burn_rate * 100.0 } else if burn_rate <= 2.0 { 100.0 - (burn_rate - 0.5) * 20.0 } else { (70.0 - (burn_rate - 2.0) * 10.0).max(0.0) };
    let tx_s = if tx_volume == 0 { 0.0 } else { ((tx_volume as f64).ln() / (1_000_000f64).ln() * 100.0).min(100.0) };
    let ac_s = if new_accounts == 0 { 0.0 } else { ((new_accounts as f64).ln() / (100_000f64).ln() * 100.0).min(100.0) };
    let as_ = tx_s * 0.7 + ac_s * 0.3;
    NetworkHealthScore { validator_score: vs, economic_score: es, activity_score: as_, composite: vs * 0.30 + es * 0.30 + as_ * 0.40 }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn test_healthy() { let s = calculate_health(5.0, 0.3, 50_000, 1_000); assert!(s.composite > 60.0 && s.composite <= 100.0); }
    #[test] fn test_dead() { let s = calculate_health(-10.0, 0.0, 0, 0); assert!(s.composite < 20.0); }
    #[test] fn test_booming() { let s = calculate_health(10.0, 0.5, 1_000_000, 100_000); assert!(s.composite > 90.0); }
    #[test] fn test_bounded() { let s = calculate_health(100.0, 100.0, u64::MAX, u64::MAX); assert!(s.composite <= 100.0); }
    #[test] fn test_weights() { let s = calculate_health(0.0, 0.0, 0, 0); assert!((s.composite - (s.validator_score * 0.30 + s.economic_score * 0.30 + s.activity_score * 0.40)).abs() < f64::EPSILON); }
    #[test] fn test_growth_penalty() { let g = calculate_health(5.0, 0.3, 10_000, 500); let b = calculate_health(-5.0, 0.3, 10_000, 500); assert!(g.validator_score > b.validator_score); }
    #[test] fn test_zeros() { let s = calculate_health(0.0, 0.0, 0, 0); assert!((s.validator_score - 40.0).abs() < f64::EPSILON); assert!((s.economic_score - 50.0).abs() < f64::EPSILON); }
}
