use serde::{Deserialize, Serialize};
use borsh::{BorshDeserialize, BorshSerialize};

use crate::tier::HolderTier;
use crate::token::UNITS_PER_COMME;

/// Rich account state with tier tracking, grace balance, and activity timestamp.
/// Eventually replaces the simpler `AccountRecord` in `store.rs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct AccountState {
    /// Balance in raw units (1 COMME = 10^8 raw units).
    pub balance: u64,
    /// Transaction sequence number.
    pub nonce: u64,
    /// Current holder tier derived from balance.
    pub tier: HolderTier,
    /// Accumulated grace balance-seconds for contributor grace period tracking.
    pub grace_balance_secs: u64,
    /// Last activity timestamp (unix seconds).
    pub last_active: u64,
}

impl AccountState {
    /// Create a new account with zero defaults.
    pub fn new() -> Self {
        Self {
            balance: 0,
            nonce: 0,
            tier: HolderTier::None,
            grace_balance_secs: 0,
            last_active: 0,
        }
    }

    /// Recalculate the holder tier from the current balance.
    /// Converts raw units to whole COMME, then delegates to `HolderTier::from_balance`.
    pub fn recalculate_tier(&mut self) {
        let whole_comme = self.balance / UNITS_PER_COMME;
        self.tier = HolderTier::from_balance(whole_comme);
    }
}

impl Default for AccountState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_account_defaults() {
        let account = AccountState::new();
        assert_eq!(account.balance, 0);
        assert_eq!(account.nonce, 0);
        assert_eq!(account.tier, HolderTier::None);
        assert_eq!(account.grace_balance_secs, 0);
        assert_eq!(account.last_active, 0);
    }

    #[test]
    fn test_recalculate_tier() {
        let mut account = AccountState::new();

        // Zero balance -> None
        account.recalculate_tier();
        assert_eq!(account.tier, HolderTier::None);

        // 5 COMME -> Base
        account.balance = 5 * UNITS_PER_COMME;
        account.recalculate_tier();
        assert_eq!(account.tier, HolderTier::Base);

        // 10 COMME -> Storage
        account.balance = 10 * UNITS_PER_COMME;
        account.recalculate_tier();
        assert_eq!(account.tier, HolderTier::Storage);

        // 25 COMME -> Compute
        account.balance = 25 * UNITS_PER_COMME;
        account.recalculate_tier();
        assert_eq!(account.tier, HolderTier::Compute);

        // 100 COMME -> Full
        account.balance = 100 * UNITS_PER_COMME;
        account.recalculate_tier();
        assert_eq!(account.tier, HolderTier::Full);
    }

    #[test]
    fn test_tier_boundaries() {
        let mut account = AccountState::new();

        // 0 COMME (exact boundary) -> None
        account.balance = 0;
        account.recalculate_tier();
        assert_eq!(account.tier, HolderTier::None);

        // Just under 1 COMME in raw units -> still None (truncates to 0 whole)
        account.balance = UNITS_PER_COMME - 1;
        account.recalculate_tier();
        assert_eq!(account.tier, HolderTier::None);

        // Exactly 1 COMME -> Base
        account.balance = 1 * UNITS_PER_COMME;
        account.recalculate_tier();
        assert_eq!(account.tier, HolderTier::Base);

        // Just under 10 COMME -> Base
        account.balance = 10 * UNITS_PER_COMME - 1;
        account.recalculate_tier();
        assert_eq!(account.tier, HolderTier::Base);

        // Exactly 10 COMME -> Storage
        account.balance = 10 * UNITS_PER_COMME;
        account.recalculate_tier();
        assert_eq!(account.tier, HolderTier::Storage);

        // Just under 20 COMME -> Storage
        account.balance = 20 * UNITS_PER_COMME - 1;
        account.recalculate_tier();
        assert_eq!(account.tier, HolderTier::Storage);

        // Exactly 20 COMME -> Compute
        account.balance = 20 * UNITS_PER_COMME;
        account.recalculate_tier();
        assert_eq!(account.tier, HolderTier::Compute);

        // Just under 33 COMME -> Compute
        account.balance = 33 * UNITS_PER_COMME - 1;
        account.recalculate_tier();
        assert_eq!(account.tier, HolderTier::Compute);

        // Exactly 33 COMME -> Full
        account.balance = 33 * UNITS_PER_COMME;
        account.recalculate_tier();
        assert_eq!(account.tier, HolderTier::Full);
    }
}
