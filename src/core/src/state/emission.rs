use serde::{Deserialize, Serialize};
use borsh::{BorshDeserialize, BorshSerialize};

use crate::token::TOTAL_SUPPLY;

/// Tracks global emission and burn accounting for the COMME token supply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct EmissionState {
    /// Total raw units emitted so far.
    pub total_emitted: u64,
    /// Total raw units burned so far.
    pub total_burned: u64,
    /// Raw units remaining to be emitted.
    pub remaining_supply: u64,
    /// Current emission epoch.
    pub current_epoch: u64,
}

impl EmissionState {
    /// Create a new emission state with full supply remaining.
    pub fn new() -> Self {
        Self {
            total_emitted: 0,
            total_burned: 0,
            remaining_supply: TOTAL_SUPPLY,
            current_epoch: 0,
        }
    }

    /// Record an emission of `amount` raw units.
    /// Saturating arithmetic prevents overflow/underflow.
    pub fn record_emission(&mut self, amount: u64) {
        self.total_emitted = self.total_emitted.saturating_add(amount);
        self.remaining_supply = self.remaining_supply.saturating_sub(amount);
    }

    /// Record a burn of `amount` raw units.
    /// Burns reduce circulating supply, not remaining-to-emit.
    pub fn record_burn(&mut self, amount: u64) {
        self.total_burned = self.total_burned.saturating_add(amount);
    }

    /// Circulating supply = total_emitted - total_burned (saturating).
    pub fn circulating_supply(&self) -> u64 {
        self.total_emitted.saturating_sub(self.total_burned)
    }
}

impl Default for EmissionState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initial_emission_state() {
        let state = EmissionState::new();
        assert_eq!(state.remaining_supply, TOTAL_SUPPLY);
        assert_eq!(state.total_emitted, 0);
        assert_eq!(state.total_burned, 0);
        assert_eq!(state.current_epoch, 0);
        assert_eq!(state.circulating_supply(), 0);
    }

    #[test]
    fn test_emit_reduces_remaining() {
        let mut state = EmissionState::new();
        let amount = 1_000_000;

        state.record_emission(amount);

        assert_eq!(state.total_emitted, amount);
        assert_eq!(state.remaining_supply, TOTAL_SUPPLY - amount);
        assert_eq!(state.circulating_supply(), amount);
    }

    #[test]
    fn test_burn_tracking() {
        let mut state = EmissionState::new();
        let emitted = 5_000_000;
        let burned = 1_000_000;

        state.record_emission(emitted);
        state.record_burn(burned);

        assert_eq!(state.total_burned, burned);
        // Burns do NOT affect remaining_supply
        assert_eq!(state.remaining_supply, TOTAL_SUPPLY - emitted);
        assert_eq!(state.circulating_supply(), emitted - burned);
    }
}
