//! Milestone burn triggers (#46)
use crate::token::Amount;

#[derive(Debug, Clone)]
pub struct MilestoneConfig { pub validator_count_thresholds: Vec<(u64, u64)> }
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MilestoneBurn { pub milestone_id: u64, pub burn_amount: Amount, pub description: String }

impl MilestoneConfig {
    pub fn default_config() -> Self {
        Self { validator_count_thresholds: vec![(1_000, 1_000_000), (10_000, 10_000_000), (100_000, 50_000_000), (1_000_000, 100_000_000)] }
    }
}

pub fn check_milestone(current_validators: u64, config: &MilestoneConfig) -> Option<MilestoneBurn> {
    let mut result = None;
    for (idx, &(threshold, burn_comme)) in config.validator_count_thresholds.iter().enumerate() {
        if current_validators >= threshold {
            result = Some(MilestoneBurn { milestone_id: idx as u64, burn_amount: Amount::from_comme(burn_comme), description: format!("Milestone: {} validators — burn {} COMME", threshold, burn_comme) });
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn test_below() { assert!(check_milestone(999, &MilestoneConfig::default_config()).is_none()); }
    #[test] fn test_1k() { let b = check_milestone(1_000, &MilestoneConfig::default_config()).unwrap(); assert_eq!(b.milestone_id, 0); assert_eq!(b.burn_amount, Amount::from_comme(1_000_000)); }
    #[test] fn test_10k() { let b = check_milestone(10_000, &MilestoneConfig::default_config()).unwrap(); assert_eq!(b.milestone_id, 1); }
    #[test] fn test_100k() { let b = check_milestone(100_000, &MilestoneConfig::default_config()).unwrap(); assert_eq!(b.milestone_id, 2); }
    #[test] fn test_1m() { let b = check_milestone(1_000_000, &MilestoneConfig::default_config()).unwrap(); assert_eq!(b.milestone_id, 3); }
    #[test] fn test_between() { let b = check_milestone(5_000, &MilestoneConfig::default_config()).unwrap(); assert_eq!(b.milestone_id, 0); }
    #[test] fn test_custom() { let c = MilestoneConfig { validator_count_thresholds: vec![(100, 500)] }; assert!(check_milestone(99, &c).is_none()); assert_eq!(check_milestone(100, &c).unwrap().burn_amount, Amount::from_comme(500)); }
}
