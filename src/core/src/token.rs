use serde::{Deserialize, Serialize};
use borsh::{BorshDeserialize, BorshSerialize};

/// Total supply: 2 billion $COMME, represented in smallest unit (1 COMME = 10^8 units).
pub const DECIMALS: u8 = 8;
pub const UNITS_PER_COMME: u64 = 100_000_000;
pub const TOTAL_SUPPLY: u64 = 2_000_000_000 * UNITS_PER_COMME;

/// Token amount in smallest units. All arithmetic is done in these units
/// to avoid floating point. 1 $COMME = 10^8 units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash,
         Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct Amount(u64);

impl Amount {
    pub const ZERO: Self = Self(0);

    /// Create from raw smallest units.
    pub const fn from_raw(units: u64) -> Self {
        Self(units)
    }

    /// Create from whole $COMME (e.g., 33 COMME).
    pub const fn from_comme(whole: u64) -> Self {
        Self(whole * UNITS_PER_COMME)
    }

    pub const fn raw(&self) -> u64 {
        self.0
    }

    /// Returns whole $COMME (truncated).
    pub const fn whole_comme(&self) -> u64 {
        self.0 / UNITS_PER_COMME
    }

    pub fn checked_add(self, other: Self) -> Option<Self> {
        self.0.checked_add(other.0).map(Self)
    }

    pub fn checked_sub(self, other: Self) -> Option<Self> {
        self.0.checked_sub(other.0).map(Self)
    }

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
}
