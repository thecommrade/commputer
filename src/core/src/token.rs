use serde::{Deserialize, Serialize};
use borsh::{BorshDeserialize, BorshSerialize};

/// Total supply: 2 billion $COMME, represented in smallest unit (1 COMME = 10^8 units).
/// Number of decimal places in the smallest unit.
pub const DECIMALS: u8 = 8;
/// Raw units per 1 $COMME (10^8).
pub const UNITS_PER_COMME: u64 = 100_000_000;
/// Total fixed supply: 2 billion $COMME in raw units.
pub const TOTAL_SUPPLY: u64 = 2_000_000_000 * UNITS_PER_COMME;

/// 51% of network compute is reserved for communal products (storage, communication, AI,
/// Humanities Archive). Also serves as emergency safeguard for user data protection.
/// This constant is enforced in the compute job routing system.
pub const FLAGSHIP_COMPUTE_SHARE: u64 = 51;

/// 49% of network compute is available for holder burst compute jobs.
/// Split equally among qualifying holders per tier. No whale advantages.
pub const HOLDER_COMPUTE_SHARE: u64 = 49;

/// Dynamic network reserve — NOT part of the 51/49 split, subtracted before.
/// Formula: reserve_pct = RESERVE_MIN + (RESERVE_RANGE × churn_rate)
/// where churn_rate = (validators_joined + validators_left) / total_validators in last epoch.
/// Purpose: absorb sudden capacity changes without disrupting active products.
/// More volatile = more reserve. Stable = less reserve.
pub const RESERVE_MIN_PERCENT: u64 = 5;
pub const RESERVE_MAX_PERCENT: u64 = 15;
pub const RESERVE_RANGE_PERCENT: u64 = 10; // RESERVE_MAX - RESERVE_MIN

/// Calculate dynamic reserve percentage based on validator churn rate (0.0 to 1.0).
pub fn dynamic_reserve_percent(churn_rate: f64) -> u64 {
    let churn_clamped = churn_rate.min(1.0).max(0.0);
    let reserve = RESERVE_MIN_PERCENT as f64 + (RESERVE_RANGE_PERCENT as f64 * churn_clamped);
    (reserve.round() as u64).min(RESERVE_MAX_PERCENT)
}

/// Diversity bonus multipliers based on number of active proof channels.
/// A well-rounded desktop contributing across all 5 channels earns a small bonus.
/// The bonus is intentionally modest — 5% max — so that missing a channel
/// (e.g., no GPU) doesn't feel like punishment.
pub const DIVERSITY_MULTIPLIER: [u64; 6] = [
    100, // 0 channels: 1.00x (shouldn't happen)
    100, // 1 channel:  1.00x
    101, // 2 channels: 1.01x
    102, // 3 channels: 1.02x
    103, // 4 channels: 1.03x
    105, // 5 channels: 1.05x
]; // Values are percentages: divide by 100 to get multiplier

/// Reference (gold-standard) benchmark scores per channel.
/// These represent a median desktop in 2026: the ceiling for reward scoring.
/// Scoring above these values earns the same reward, not more.
/// Values are on a 0-100 scale matching EpochProofSummary scores.
pub const REFERENCE_SCORES: [u32; 5] = [
    100, // Processing (CPU): reference desktop at 100%
    100, // GPU: reference desktop at 100%
    100, // Storage: reference desktop at 100%
    100, // RAM: reference desktop at 100%
    100, // Bandwidth: reference desktop at 100%
];

/// Cost of one reference-node-equivalent for one year of burst compute, in raw COMME units.
/// Pegged to 0.3225 troy oz / 10.03g of gold at 2026 median currency.
/// At ~33 COMME/year reference yield, burst compute costs 33 COMME per ref-node-year.
pub const BURST_COMPUTE_ANNUAL_COST: u64 = 33 * UNITS_PER_COMME;

/// Cap a channel score at the reference (gold-standard) level.
/// Nodes that benchmark above reference earn the same, not more.
pub fn cap_at_reference(channel_idx: usize, score: u32) -> u32 {
    let reference = REFERENCE_SCORES.get(channel_idx).copied().unwrap_or(100);
    score.min(reference)
}

/// Token amount in smallest units. All arithmetic is done in these units
/// to avoid floating point. 1 $COMME = 10^8 units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash,
         Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct Amount(u64);

impl Amount {
    /// Zero amount constant.
    pub const ZERO: Self = Self(0);

    /// Create from raw smallest units.
    pub const fn from_raw(units: u64) -> Self {
        Self(units)
    }

    /// Create from whole $COMME (e.g., 33 COMME).
    pub const fn from_comme(whole: u64) -> Self {
        Self(whole * UNITS_PER_COMME)
    }

    /// Returns the raw unit value.
    pub const fn raw(&self) -> u64 {
        self.0
    }

    /// Returns whole $COMME (truncated).
    pub const fn whole_comme(&self) -> u64 {
        self.0 / UNITS_PER_COMME
    }

    /// Add two amounts, returning `None` on overflow.
    pub fn checked_add(self, other: Self) -> Option<Self> {
        self.0.checked_add(other.0).map(Self)
    }

    /// Subtract, returning `None` if the result would be negative.
    pub fn checked_sub(self, other: Self) -> Option<Self> {
        self.0.checked_sub(other.0).map(Self)
    }

    /// Subtract, clamping at zero instead of underflowing.
    pub fn saturating_sub(self, other: Self) -> Self {
        Self(self.0.saturating_sub(other.0))
    }
}

impl std::fmt::Display for Amount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let whole = self.0 / UNITS_PER_COMME;
        let frac = self.0 % UNITS_PER_COMME;
        if frac == 0 {
            write!(f, "{} COMME", whole)
        } else {
            write!(f, "{}.{:08} COMME", whole, frac)
        }
    }
}

/// Formats an `Amount` with thousand separators (commas) for readability.
///
/// # Examples
/// ```
/// use commputer_core::token::{Amount, format_with_commas};
/// assert_eq!(format_with_commas(Amount::from_comme(1234567)), "1,234,567 COMME");
/// ```
pub fn format_with_commas(amount: Amount) -> String {
    let whole = amount.0 / UNITS_PER_COMME;
    let frac = amount.0 % UNITS_PER_COMME;

    // Format whole part with commas
    let whole_str = whole.to_string();
    let mut with_commas = String::new();
    for (i, c) in whole_str.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            with_commas.push(',');
        }
        with_commas.push(c);
    }
    let whole_formatted: String = with_commas.chars().rev().collect();

    if frac == 0 {
        format!("{} COMME", whole_formatted)
    } else {
        format!("{}.{:08} COMME", whole_formatted, frac)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn total_supply_fits_u64() {
        // 2B * 10^8 = 2 * 10^17, well within u64::MAX (~1.8 * 10^19)
        assert!(TOTAL_SUPPLY < u64::MAX);
    }

    #[test]
    fn amount_arithmetic() {
        let a = Amount::from_comme(33);
        let b = Amount::from_comme(10);
        assert_eq!(a.checked_sub(b), Some(Amount::from_comme(23)));
        assert_eq!(b.checked_sub(a), None);
    }

    #[test]
    fn display() {
        assert_eq!(Amount::from_comme(33).to_string(), "33 COMME");
        assert_eq!(Amount::from_raw(100_000_001).to_string(), "1.00000001 COMME");
    }

    #[test]
    fn checked_add_never_overflows() {
        let max = Amount::from_raw(u64::MAX);
        assert_eq!(max.checked_add(Amount::from_raw(1)), None);
        assert_eq!(max.checked_add(Amount::ZERO), Some(max));
    }

    #[test]
    fn checked_sub_never_underflows() {
        assert_eq!(Amount::ZERO.checked_sub(Amount::from_raw(1)), None);
        assert_eq!(Amount::from_raw(1).checked_sub(Amount::from_raw(1)), Some(Amount::ZERO));
    }

    #[test]
    fn total_supply_add_never_exceeds_u64() {
        // Even adding total supply to itself shouldn't panic (use checked).
        let supply = Amount::from_raw(TOTAL_SUPPLY);
        assert!(supply.checked_add(supply).is_some()); // 4*10^17 < u64::MAX
    }

    #[test]
    fn saturating_sub_floors_at_zero() {
        assert_eq!(Amount::from_raw(5).saturating_sub(Amount::from_raw(10)), Amount::ZERO);
    }

    #[test]
    fn from_comme_and_back() {
        for whole in [0, 1, 100, 1_000_000, 2_000_000_000u64] {
            let a = Amount::from_comme(whole);
            assert_eq!(a.whole_comme(), whole);
        }
    }

    #[test]
    fn supply_invariant_emission_plus_remaining_equals_total() {
        // Simulate emission: any amount emitted + remaining = total.
        for emitted in [0, 1, TOTAL_SUPPLY / 2, TOTAL_SUPPLY - 1, TOTAL_SUPPLY] {
            let remaining = TOTAL_SUPPLY - emitted;
            assert_eq!(emitted + remaining, TOTAL_SUPPLY);
        }
    }

    #[test]
    fn format_with_commas_whole() {
        assert_eq!(format_with_commas(Amount::from_comme(0)), "0 COMME");
        assert_eq!(format_with_commas(Amount::from_comme(999)), "999 COMME");
        assert_eq!(format_with_commas(Amount::from_comme(1000)), "1,000 COMME");
        assert_eq!(format_with_commas(Amount::from_comme(1234567)), "1,234,567 COMME");
        assert_eq!(
            format_with_commas(Amount::from_comme(2_000_000_000)),
            "2,000,000,000 COMME"
        );
    }

    #[test]
    fn format_with_commas_fractional() {
        assert_eq!(
            format_with_commas(Amount::from_raw(100_000_001)),
            "1.00000001 COMME"
        );
        assert_eq!(
            format_with_commas(Amount::from_raw(123_456_789_000_000)),
            "1,234,567.89000000 COMME"
        );
    }
}
