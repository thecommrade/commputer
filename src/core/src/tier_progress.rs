//! Tier progress display (#41)
use crate::tier::HolderTier;

#[derive(Debug, Clone, PartialEq)]
pub struct TierProgress {
    pub current_tier: String,
    pub next_tier: Option<String>,
    pub progress_percent: f64,
    pub amount_to_next: u64,
}

pub fn tier_progress(balance: u64) -> TierProgress {
    let current = HolderTier::from_balance(balance);
    let (current_name, current_threshold, next_name, next_threshold) = match current {
        HolderTier::None => ("None", 0u64, Some("Holder"), Some(HolderTier::BASE_THRESHOLD)),
        HolderTier::Base => ("Holder", HolderTier::BASE_THRESHOLD, Some("Supporter"), Some(HolderTier::STORAGE_THRESHOLD)),
        HolderTier::Storage => ("Supporter", HolderTier::STORAGE_THRESHOLD, Some("Advocate"), Some(HolderTier::COMPUTE_THRESHOLD)),
        HolderTier::Compute => ("Advocate", HolderTier::COMPUTE_THRESHOLD, Some("Champion"), Some(HolderTier::FULL_THRESHOLD)),
        HolderTier::Full => ("Champion", HolderTier::FULL_THRESHOLD, None, None),
    };
    match (next_name, next_threshold) {
        (Some(next), Some(next_thresh)) => {
            let range = next_thresh - current_threshold;
            let progress = balance.saturating_sub(current_threshold);
            let percent = if range > 0 { (progress as f64 / range as f64 * 100.0).min(100.0) } else { 100.0 };
            TierProgress { current_tier: current_name.to_string(), next_tier: Some(next.to_string()), progress_percent: percent, amount_to_next: next_thresh.saturating_sub(balance) }
        }
        _ => TierProgress { current_tier: current_name.to_string(), next_tier: None, progress_percent: 100.0, amount_to_next: 0 },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn test_no_balance() { let p = tier_progress(0); assert_eq!(p.current_tier, "None"); assert_eq!(p.amount_to_next, 1); }
    #[test] fn test_holder() { let p = tier_progress(1); assert_eq!(p.current_tier, "Holder"); assert_eq!(p.amount_to_next, 9); }
    #[test] fn test_supporter() { let p = tier_progress(15); assert_eq!(p.current_tier, "Supporter"); assert_eq!(p.amount_to_next, 5); }
    #[test] fn test_advocate() { let p = tier_progress(20); assert_eq!(p.current_tier, "Advocate"); assert_eq!(p.amount_to_next, 13); }
    #[test] fn test_champion() { let p = tier_progress(33); assert_eq!(p.current_tier, "Champion"); assert_eq!(p.next_tier, None); assert_eq!(p.amount_to_next, 0); }
    #[test] fn test_high_balance() { let p = tier_progress(10000); assert_eq!(p.current_tier, "Champion"); }
    #[test] fn test_progress_pct() { let p = tier_progress(5); let expected = 4.0 / 9.0 * 100.0; assert!((p.progress_percent - expected).abs() < 0.01); }
}
