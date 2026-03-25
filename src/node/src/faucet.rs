use std::collections::HashMap;
use std::sync::Mutex;
use commputer_core::token::{Amount, UNITS_PER_COMME};

/// Amount dispensed per faucet claim: 10 COMME.
pub const FAUCET_AMOUNT: u64 = 10 * UNITS_PER_COMME;

/// Error returned when a faucet request is rejected.
#[derive(Debug)]
pub enum FaucetError {
    /// The address has already claimed within the current epoch.
    RateLimited { address: String, epoch: u64 },
}

impl std::fmt::Display for FaucetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FaucetError::RateLimited { address, epoch } => {
                write!(f, "address {} already claimed in epoch {}", address, epoch)
            }
        }
    }
}

/// Thread-safe faucet state that rate-limits claims to 1 per address per epoch.
pub struct FaucetState {
    /// Current epoch number.
    current_epoch: Mutex<u64>,
    /// Map from address to the last epoch in which they claimed.
    claims: Mutex<HashMap<String, u64>>,
}

impl FaucetState {
    /// Create a new FaucetState starting at the given epoch.
    pub fn new(initial_epoch: u64) -> Self {
        Self {
            current_epoch: Mutex::new(initial_epoch),
            claims: Mutex::new(HashMap::new()),
        }
    }

    /// Advance the faucet to a new epoch.
    pub fn set_epoch(&self, epoch: u64) {
        let mut current = self.current_epoch.lock().unwrap();
        *current = epoch;
    }

    /// Get the current epoch.
    pub fn epoch(&self) -> u64 {
        *self.current_epoch.lock().unwrap()
    }
}

/// Handle a faucet request for the given address.
/// Returns the dispensed amount on success, or an error if rate-limited.
pub fn handle_faucet_request(address: &str, state: &FaucetState) -> Result<Amount, FaucetError> {
    let epoch = state.epoch();
    let mut claims = state.claims.lock().unwrap();

    if let Some(&last_epoch) = claims.get(address) {
        if last_epoch >= epoch {
            return Err(FaucetError::RateLimited {
                address: address.to_string(),
                epoch,
            });
        }
    }

    claims.insert(address.to_string(), epoch);
    Ok(Amount::from_raw(FAUCET_AMOUNT))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn faucet_dispenses_correct_amount() {
        let state = FaucetState::new(1);
        let result = handle_faucet_request("comme:abc123", &state).unwrap();
        assert_eq!(result, Amount::from_comme(10));
    }

    #[test]
    fn faucet_rate_limits_same_epoch() {
        let state = FaucetState::new(1);
        handle_faucet_request("comme:abc123", &state).unwrap();
        let result = handle_faucet_request("comme:abc123", &state);
        assert!(result.is_err());
    }

    #[test]
    fn faucet_allows_claim_in_new_epoch() {
        let state = FaucetState::new(1);
        handle_faucet_request("comme:abc123", &state).unwrap();
        state.set_epoch(2);
        let result = handle_faucet_request("comme:abc123", &state);
        assert!(result.is_ok());
    }

    #[test]
    fn faucet_different_addresses_independent() {
        let state = FaucetState::new(1);
        handle_faucet_request("comme:aaa", &state).unwrap();
        let result = handle_faucet_request("comme:bbb", &state);
        assert!(result.is_ok());
    }
}
