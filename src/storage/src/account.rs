use serde::{Deserialize, Serialize};
use borsh::{BorshDeserialize, BorshSerialize};
use sha2::{Sha256, Digest};
use commputer_core::identity::Address;
use commputer_core::token::Amount;
use commputer_core::tier::HolderTier;
use commputer_core::compliance::ComplianceStatus;

/// An account on the Commputer network.
/// Every address that has interacted with the chain has an account.
#[derive(Debug, Clone, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct Account {
    pub address: Address,
    /// Current $COMME balance.
    pub balance: Amount,
    /// Transaction nonce (for replay protection).
    pub nonce: u64,
    /// Whether this address is a registered validator.
    pub is_validator: bool,
    /// Compliance status (only relevant if is_validator).
    pub compliance: ComplianceStatus,
    /// Cumulative uptime in seconds (validators only).
    pub cumulative_uptime_secs: u64,
    /// Grace period balance in seconds (drains while offline, refills at 2:1 while online).
    pub grace_balance_secs: u64,
    /// Total $COMME earned through mining (lifetime).
    pub total_mined: Amount,
    /// Total $COMME burned by this account (burst compute, etc.).
    pub total_burned: Amount,
    /// Storage will contact hashes (if configured).
    pub will_contacts: Vec<[u8; 32]>,
}

impl Account {
    /// Create a new empty account.
    pub fn new(address: Address) -> Self {
        Self {
            address,
            balance: Amount::ZERO,
            nonce: 0,
            is_validator: false,
            compliance: ComplianceStatus::Compliant,
            cumulative_uptime_secs: 0,
            grace_balance_secs: 0,
            total_mined: Amount::ZERO,
            total_burned: Amount::ZERO,
            will_contacts: Vec::new(),
        }
    }

    /// Current holder tier based on balance.
    pub fn tier(&self) -> HolderTier {
        HolderTier::from_balance(self.balance.whole_comme())
    }

    /// Whether this account has access to the flagship product.
    /// Either holds 1+ COMME or is an active full-desktop contributor.
    pub fn has_flagship_access(&self) -> bool {
        self.balance.whole_comme() >= 1 || self.is_full_contributor()
    }

    /// Whether this account is contributing a full desktop (100% of reference node).
    /// Determined by validator status + compliance.
    /// Actual contribution percentage is tracked in the validator identity on-chain.
    pub fn is_full_contributor(&self) -> bool {
        self.is_validator && self.compliance == ComplianceStatus::Compliant
    }

    /// Maximum grace period: 10 years.
    pub const MAX_GRACE_SECS: u64 = 10 * 365 * 24 * 3600;

    /// Update grace balance. Drains 1:1 while offline, refills 2:1 while online.
    pub fn drain_grace(&mut self, offline_secs: u64) {
        self.grace_balance_secs = self.grace_balance_secs.saturating_sub(offline_secs);
    }

    pub fn refill_grace(&mut self, online_secs: u64) {
        // Refill at 2:1 (5 days online restores 10 days drained).
        let refill = online_secs * 2;
        self.grace_balance_secs = (self.grace_balance_secs + refill)
            .min(self.cumulative_uptime_secs)
            .min(Self::MAX_GRACE_SECS);
    }
}

/// In-memory account store. Will be backed by RocksDB in production.
#[derive(Debug, Default)]
pub struct AccountStore {
    accounts: std::collections::HashMap<Address, Account>,
}

impl AccountStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, address: &Address) -> Option<&Account> {
        self.accounts.get(address)
    }

    pub fn get_mut(&mut self, address: &Address) -> Option<&mut Account> {
        self.accounts.get_mut(address)
    }

    pub fn get_or_create(&mut self, address: Address) -> &mut Account {
        self.accounts.entry(address).or_insert_with(|| Account::new(address))
    }

    pub fn put(&mut self, account: Account) {
        self.accounts.insert(account.address, account);
    }

    /// Count holders at or above a given tier.
    pub fn count_at_tier(&self, tier: HolderTier) -> u64 {
        self.accounts.values()
            .filter(|a| a.tier() >= tier)
            .count() as u64
    }

    /// Total accounts.
    pub fn len(&self) -> usize {
        self.accounts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty()
    }

    /// Iterate over all accounts.
    pub fn iter(&self) -> impl Iterator<Item = &Account> {
        self.accounts.values()
    }

    /// Compute the merkle root of all account states.
    /// Accounts are sorted by address for deterministic ordering,
    /// then hashed into a binary merkle tree.
    pub fn compute_state_root(&self) -> [u8; 32] {
        if self.accounts.is_empty() {
            return [0u8; 32];
        }

        // Sort accounts by address for deterministic ordering.
        let mut sorted: Vec<&Account> = self.accounts.values().collect();
        sorted.sort_by_key(|a| a.address.0);

        // Hash each account into a leaf.
        let leaves: Vec<[u8; 32]> = sorted.iter().map(|acct| {
            let encoded = borsh::to_vec(acct).expect("account serialization");
            let hash = Sha256::digest(&encoded);
            let mut out = [0u8; 32];
            out.copy_from_slice(&hash);
            out
        }).collect();

        merkle_root(&leaves)
    }
}

/// Simple binary merkle root computation.
fn merkle_root(leaves: &[[u8; 32]]) -> [u8; 32] {
    if leaves.is_empty() {
        return [0u8; 32];
    }
    if leaves.len() == 1 {
        return leaves[0];
    }
    let mut next = Vec::with_capacity((leaves.len() + 1) / 2);
    for pair in leaves.chunks(2) {
        let mut hasher = Sha256::new();
        hasher.update(pair[0]);
        if pair.len() == 2 {
            hasher.update(pair[1]);
        } else {
            hasher.update(pair[0]);
        }
        let hash = hasher.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(&hash);
        next.push(out);
    }
    merkle_root(&next)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_address(n: u8) -> Address {
        let mut a = [0u8; 32];
        a[0] = n;
        Address(a)
    }

    #[test]
    fn new_account_defaults() {
        let acct = Account::new(test_address(1));
        assert_eq!(acct.balance, Amount::ZERO);
        assert_eq!(acct.tier(), HolderTier::None);
        assert!(!acct.has_flagship_access());
        assert!(!acct.is_validator);
    }

    #[test]
    fn tier_from_balance() {
        let mut acct = Account::new(test_address(1));
        acct.balance = Amount::from_comme(1);
        assert_eq!(acct.tier(), HolderTier::Base);
        assert!(acct.has_flagship_access());

        acct.balance = Amount::from_comme(33);
        assert_eq!(acct.tier(), HolderTier::Full);
    }

    #[test]
    fn grace_period_drain_and_refill() {
        let mut acct = Account::new(test_address(1));
        acct.cumulative_uptime_secs = 365 * 24 * 3600; // 1 year
        acct.grace_balance_secs = 365 * 24 * 3600;     // Full grace

        // Offline for 10 days.
        acct.drain_grace(10 * 24 * 3600);
        let expected = 365 * 24 * 3600 - 10 * 24 * 3600;
        assert_eq!(acct.grace_balance_secs, expected);

        // Online for 5 days → restores 10 days (2:1 ratio).
        acct.refill_grace(5 * 24 * 3600);
        assert_eq!(acct.grace_balance_secs, 365 * 24 * 3600); // Back to full
    }

    #[test]
    fn account_store_tier_counting() {
        let mut store = AccountStore::new();
        let mut a1 = Account::new(test_address(1));
        a1.balance = Amount::from_comme(33);
        store.put(a1);

        let mut a2 = Account::new(test_address(2));
        a2.balance = Amount::from_comme(10);
        store.put(a2);

        let mut a3 = Account::new(test_address(3));
        a3.balance = Amount::from_comme(5);
        store.put(a3);

        assert_eq!(store.count_at_tier(HolderTier::Full), 1);
        assert_eq!(store.count_at_tier(HolderTier::Storage), 2); // 33 + 10
        assert_eq!(store.count_at_tier(HolderTier::Base), 3);    // All 3
    }

    #[test]
    fn grace_caps_at_10_years() {
        let mut acct = Account::new(test_address(1));
        acct.cumulative_uptime_secs = 20 * 365 * 24 * 3600; // 20 years
        acct.grace_balance_secs = 20 * 365 * 24 * 3600;
        // Grace should cap at 10 years
        acct.refill_grace(1);
        assert!(acct.grace_balance_secs <= Account::MAX_GRACE_SECS);
    }

    #[test]
    fn grace_drains_to_zero_not_negative() {
        let mut acct = Account::new(test_address(1));
        acct.cumulative_uptime_secs = 100;
        acct.grace_balance_secs = 100;
        acct.drain_grace(200); // More than balance
        assert_eq!(acct.grace_balance_secs, 0);
    }

    #[test]
    fn state_root_deterministic() {
        let mut store = AccountStore::new();
        let mut a1 = Account::new(test_address(1));
        a1.balance = Amount::from_comme(100);
        store.put(a1);
        let mut a2 = Account::new(test_address(2));
        a2.balance = Amount::from_comme(50);
        store.put(a2);

        let root1 = store.compute_state_root();
        let root2 = store.compute_state_root();
        assert_eq!(root1, root2, "state root must be deterministic");
        assert_ne!(root1, [0u8; 32], "state root must not be zero with accounts");
    }

    #[test]
    fn state_root_changes_on_balance_update() {
        let mut store = AccountStore::new();
        let mut a1 = Account::new(test_address(1));
        a1.balance = Amount::from_comme(100);
        store.put(a1.clone());
        let root_before = store.compute_state_root();

        a1.balance = Amount::from_comme(200);
        store.put(a1);
        let root_after = store.compute_state_root();
        assert_ne!(root_before, root_after, "state root must change when balance changes");
    }

    #[test]
    fn state_root_empty_is_zero() {
        let store = AccountStore::new();
        assert_eq!(store.compute_state_root(), [0u8; 32]);
    }

    #[test]
    fn refill_rate_2_to_1_exact() {
        let mut acct = Account::new(test_address(1));
        let one_year = 365 * 24 * 3600u64;
        acct.cumulative_uptime_secs = one_year;
        acct.grace_balance_secs = one_year;

        // Drain 10 days
        let ten_days = 10 * 24 * 3600u64;
        acct.drain_grace(ten_days);
        let after_drain = acct.grace_balance_secs;
        assert_eq!(after_drain, one_year - ten_days);

        // Refill 5 days (2:1 ratio should restore 10 days)
        let five_days = 5 * 24 * 3600u64;
        acct.refill_grace(five_days);
        assert_eq!(acct.grace_balance_secs, one_year);
    }
}
