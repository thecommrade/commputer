use serde::{Deserialize, Serialize};
use borsh::{BorshDeserialize, BorshSerialize};

/// Total supply: 2 billion $COMME, represented in smallest unit (1 COMME = 10^8 units).
/// Number of decimal places in the smallest unit.
pub const DECIMALS: u8 = 8;
/// Raw units per 1 $COMME (10^8).
pub const UNITS_PER_COMME: u64 = 100_000_000;
/// Total fixed supply: 2 billion $COMME in raw units.
pub const TOTAL_SUPPLY: u64 = 2_000_000_000 * UNITS_PER_COMME;

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
