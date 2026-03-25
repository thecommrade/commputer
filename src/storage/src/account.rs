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
    /// Feature 183: Last epoch this account was active (sent or received a tx).
    #[serde(default)]
    pub last_active_epoch: u64,
    /// Feature 184: Storage used by this account (in bytes).
    #[serde(default)]
    pub storage_used_bytes: u64,
    /// Feature 185: Whether this account is currently in hot storage (in-memory).
    #[serde(default = "default_true")]
    pub is_hot: bool,
    /// Feature 5: Block height at which this validator was registered.
    #[serde(default)]
    pub validator_registered_height: Option<u64>,
}

fn default_true() -> bool { true }

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
            last_active_epoch: 0,
            storage_used_bytes: 0,
            is_hot: true,
            validator_registered_height: None,
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

    /// Refill grace balance at 2:1 ratio (5 days online = 10 days restored).
    pub fn refill_grace(&mut self, online_secs: u64) {
        // Refill at 2:1 (5 days online restores 10 days drained).
        let refill = online_secs * 2;
        self.grace_balance_secs = (self.grace_balance_secs + refill)
            .min(self.cumulative_uptime_secs)
            .min(Self::MAX_GRACE_SECS);
    }

    /// Feature 184: Storage tier allocation in bytes.
    /// Base=1GB, Storage=10GB, Compute=20GB, Full=50GB, None=0.
    pub fn storage_tier_allocation(&self) -> u64 {
        match self.tier() {
            HolderTier::None => 0,
            HolderTier::Base => 1_000_000_000,         // 1 GB
            HolderTier::Storage => 10_000_000_000,     // 10 GB
            HolderTier::Compute => 20_000_000_000,     // 20 GB
            HolderTier::Full => 50_000_000_000,        // 50 GB
        }
    }

    /// Feature 184: Remaining storage quota (can be negative if over-limit).
    pub fn storage_quota_remaining(&self) -> i64 {
        self.storage_tier_allocation() as i64 - self.storage_used_bytes as i64
    }
}

/// In-memory account store. Will be backed by RocksDB in production.
#[derive(Debug, Default, Clone)]
pub struct AccountStore {
    accounts: std::collections::HashMap<Address, Account>,
}

impl AccountStore {
    /// Create an empty account store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up an account by address.
    pub fn get(&self, address: &Address) -> Option<&Account> {
        self.accounts.get(address)
    }

    /// Get a mutable reference to an account.
    pub fn get_mut(&mut self, address: &Address) -> Option<&mut Account> {
        self.accounts.get_mut(address)
    }

    /// Get or create an account for the given address.
    pub fn get_or_create(&mut self, address: Address) -> &mut Account {
        self.accounts.entry(address).or_insert_with(|| Account::new(address))
    }

    /// Insert or update an account.
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
    /// Total number of accounts.
    pub fn len(&self) -> usize {
        self.accounts.len()
    }

    /// Returns true if no accounts exist.
    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty()
    }

    /// Remove an account from the store.
    pub fn remove(&mut self, address: &Address) -> Option<Account> {
        self.accounts.remove(address)
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

    // Feature 208: Tier threshold test — accounts at exact tier boundaries
    #[test]
    fn feature_208_exact_tier_boundaries() {
        use commputer_core::token::UNITS_PER_COMME;

        let cases = vec![
            // (raw_amount, expected_tier)
            // 0.99 COMME -> None
            (UNITS_PER_COMME * 99 / 100, HolderTier::None),
            // 1.0 COMME -> Base
            (UNITS_PER_COMME, HolderTier::Base),
            // 9.99 COMME -> Base
            (UNITS_PER_COMME * 999 / 100, HolderTier::Base),
            // 10.0 COMME -> Storage
            (UNITS_PER_COMME * 10, HolderTier::Storage),
            // 19.99 COMME -> Storage
            (UNITS_PER_COMME * 1999 / 100, HolderTier::Storage),
            // 20.0 COMME -> Compute
            (UNITS_PER_COMME * 20, HolderTier::Compute),
            // 32.99 COMME -> Compute
            (UNITS_PER_COMME * 3299 / 100, HolderTier::Compute),
            // 33.0 COMME -> Full
            (UNITS_PER_COMME * 33, HolderTier::Full),
        ];

        for (raw, expected) in cases {
            let mut acct = Account::new(test_address(1));
            acct.balance = Amount::from_raw(raw);
            assert_eq!(
                acct.tier(),
                expected,
                "Balance {} raw ({} whole COMME) should be tier {:?}",
                raw,
                raw / UNITS_PER_COMME,
                expected
            );
        }
    }

    // Feature 207: Grace period comprehensive math test
    #[test]
    fn feature_207_grace_period_patterns() {
        let one_day = 24 * 3600u64;
        let one_year = 365 * one_day;

        // Pattern 1: 100% uptime for 1 year, then 6 months offline
        {
            let mut acct = Account::new(test_address(1));
            acct.cumulative_uptime_secs = one_year;
            acct.grace_balance_secs = one_year;

            // 6 months offline drains 1:1
            let six_months = one_year / 2;
            acct.drain_grace(six_months);
            assert_eq!(acct.grace_balance_secs, one_year - six_months);

            // Then 3 months online restores 6 months (2:1 refill)
            let three_months = one_year / 4;
            acct.refill_grace(three_months);
            // Should be back to full (capped by cumulative_uptime_secs)
            assert_eq!(acct.grace_balance_secs, one_year);
        }

        // Pattern 2: Weekend warrior — 5 days offline, 2 online, repeat
        {
            let mut acct = Account::new(test_address(2));
            acct.cumulative_uptime_secs = one_year;
            acct.grace_balance_secs = one_year;

            for _week in 0..52 {
                // 5 days offline (drain 1:1)
                acct.drain_grace(5 * one_day);
                // 2 days online (refill 2:1 = 4 days of grace)
                acct.refill_grace(2 * one_day);
            }
            // Net loss per week: 5 - 4 = 1 day of grace
            // After 52 weeks: lost 52 days of grace
            let expected = one_year - 52 * one_day;
            assert_eq!(acct.grace_balance_secs, expected);
        }

        // Pattern 3: 11 years online, verify cap at 10 years
        {
            let mut acct = Account::new(test_address(3));
            let eleven_years = 11 * one_year;
            acct.cumulative_uptime_secs = eleven_years;
            acct.grace_balance_secs = 0;

            // Refill with 11 years of online time
            acct.refill_grace(eleven_years);
            // Should cap at MAX_GRACE_SECS (10 years)
            assert_eq!(acct.grace_balance_secs, Account::MAX_GRACE_SECS);
            assert!(acct.grace_balance_secs <= 10 * one_year);
        }
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
