use serde::{Deserialize, Serialize};
use borsh::{BorshDeserialize, BorshSerialize};


/// Holder tier based on $COMME balance.
/// Each tier unlocks additional communal resource access.
/// At 1M remaining supply, any contribution grants full access to all tiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord,
         Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub enum HolderTier {
    /// No $COMME held. No access (unless contributing a full desktop).
    None,
    /// 1+ $COMME: Full analytics platform access.
    Base,
    /// 10+ $COMME: Communal storage allocation.
    Storage,
    /// 20+ $COMME: Communal compute allocation.
    Compute,
    /// 33+ $COMME: Full personal computer + AI access (when developed).
    Full,
}

impl HolderTier {
    /// Minimum $COMME required for each tier.
    pub const BASE_THRESHOLD: u64 = 1;
    pub const STORAGE_THRESHOLD: u64 = 10;
    pub const COMPUTE_THRESHOLD: u64 = 20;
    pub const FULL_THRESHOLD: u64 = 33;

    /// Determine tier from a wallet balance (whole $COMME).
    pub fn from_balance(whole_comme: u64) -> Self {
        if whole_comme >= Self::FULL_THRESHOLD {
            Self::Full
        } else if whole_comme >= Self::COMPUTE_THRESHOLD {
            Self::Compute
        } else if whole_comme >= Self::STORAGE_THRESHOLD {
            Self::Storage
        } else if whole_comme >= Self::BASE_THRESHOLD {
            Self::Base
        } else {
            Self::None
        }
    }

    /// When remaining supply drops below this threshold,
    /// any contribution grants full access regardless of balance.
    pub const EMERGENCY_SUPPLY_THRESHOLD: u64 = 1_000_000;
}

/// Whether a user has access via holding or contributing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessPath {
    /// Owns $COMME — permanent access at their tier even while offline.
    Holder { tier: HolderTier },
    /// Contributing a full desktop at 100% — full access while online.
    /// 15+ days contribution unlocks grace period for downtime.
    Contributor,
    /// Contributing at reduced capacity — partial access proportional to contribution.
    PartialContributor { contribution_percent: u8 },
    /// Emergency mode: supply below 1M, any contribution grants full access.
    EmergencyAccess,
}

/// Resource allocation for a specific tier.
/// The 51/49 split: 51% to flagship, 49% divided equally among tier holders.
#[derive(Debug, Clone)]
pub struct TierAllocation {
    pub tier: HolderTier,
    /// Number of holders at this tier level.
    pub holder_count: u64,
    /// Total network resource for this type (e.g., total storage in MB).
    pub total_network_resource: u64,
    /// This holder's individual share.
    pub individual_share: u64,
}

impl TierAllocation {
    /// Calculate individual share: 49% of total, divided equally among holders.
    pub fn calculate(tier: HolderTier, holder_count: u64, total_network_resource: u64) -> Self {
        let communal_pool = total_network_resource * 49 / 100;
        let individual_share = if holder_count > 0 {
            communal_pool / holder_count
        } else {
            0
        };
        Self {
            tier,
            holder_count,
            total_network_resource,
            individual_share,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_thresholds() {
        assert_eq!(HolderTier::from_balance(0), HolderTier::None);
        assert_eq!(HolderTier::from_balance(1), HolderTier::Base);
        assert_eq!(HolderTier::from_balance(9), HolderTier::Base);
        assert_eq!(HolderTier::from_balance(10), HolderTier::Storage);
        assert_eq!(HolderTier::from_balance(20), HolderTier::Compute);
        assert_eq!(HolderTier::from_balance(33), HolderTier::Full);
        assert_eq!(HolderTier::from_balance(10000), HolderTier::Full);
    }

    #[test]
    fn allocation_math() {
        // 1000 holders, 100TB total storage
        let alloc = TierAllocation::calculate(
            HolderTier::Storage,
            1000,
            100_000_000, // 100TB in MB
        );
        // 49% of 100TB = 49TB, divided by 1000 = 49GB each
        assert_eq!(alloc.individual_share, 49_000);
    }
}
