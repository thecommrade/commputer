//! Validator registry operations: register, deregister, and update validators.

use crate::error::CommpError;
use crate::identity::{Address, ValidatorIdentity};
use crate::state::store::StateStore;

/// Register a new validator in the store.
/// Returns an error if a validator with this address is already registered.
pub fn register_validator<S: StateStore>(
    store: &S,
    identity: ValidatorIdentity,
) -> Result<(), CommpError> {
    let addr = identity.address;
    if store.get_validator(&addr)?.is_some() {
        return Err(CommpError::InvalidTransaction(
            "validator already registered".to_string(),
        ));
    }
    store.set_validator(&addr, &identity)?;
    Ok(())
}

/// Remove a validator from the store.
/// Returns an error if the validator is not currently registered.
pub fn deregister_validator<S: StateStore>(
    store: &S,
    addr: &Address,
) -> Result<(), CommpError> {
    if store.get_validator(addr)?.is_none() {
        return Err(CommpError::UnknownValidator(
            format!("validator {} not registered", addr),
        ));
    }
    store.remove_validator(addr)?;
    Ok(())
}

/// Update a validator's contribution_percent.
/// Returns an error if the validator is not currently registered.
pub fn update_validator<S: StateStore>(
    store: &S,
    addr: &Address,
    contribution_percent: u8,
) -> Result<(), CommpError> {
    let mut identity = store.get_validator(addr)?.ok_or_else(|| {
        CommpError::UnknownValidator(format!("validator {} not registered", addr))
    })?;
    identity.capacity.contribution_percent = contribution_percent;
    store.set_validator(addr, &identity)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{HardwareFingerprint, ResourceCapacity};
    use crate::state::store::InMemoryStore;
    use crate::testutil::test_addr;
    use ed25519_dalek::SigningKey;

    fn test_validator(addr: Address, contribution_percent: u8) -> ValidatorIdentity {
        let signing_key = SigningKey::from_bytes(&addr.0);
        ValidatorIdentity {
            address: addr,
            public_key: signing_key.verifying_key(),
            hardware: HardwareFingerprint {
                cpu_model: "Test CPU".to_string(),
                cpu_cores: 4,
                ram_total_mb: 8192,
                gpu_model: None,
                gpu_vram_mb: None,
                storage_total_mb: 512000,
                os_family: "linux".to_string(),
                network_speed_mbps: 100,
            },
            capacity: ResourceCapacity {
                cpu_score: 100,
                gpu_score: 0,
                ram_available_mb: 4096,
                storage_available_mb: 256000,
                bandwidth_kbps: 50000,
                contribution_percent,
            },
            registered_epoch: 0,
            cumulative_uptime_secs: 0,
        }
    }

    #[test]
    fn test_register_validator() {
        let store = InMemoryStore::new();
        let addr = test_addr(10);
        let vi = test_validator(addr, 50);

        register_validator(&store, vi).unwrap();

        let got = store.get_validator(&addr).unwrap().expect("validator should exist");
        assert_eq!(got.address, addr);
        assert_eq!(got.capacity.contribution_percent, 50);
    }

    #[test]
    fn test_register_duplicate_fails() {
        let store = InMemoryStore::new();
        let addr = test_addr(11);

        register_validator(&store, test_validator(addr, 50)).unwrap();
        let result = register_validator(&store, test_validator(addr, 60));

        assert!(result.is_err());
        match result {
            Err(CommpError::InvalidTransaction(msg)) => {
                assert!(msg.contains("already registered"), "unexpected message: {}", msg);
            }
            other => panic!("expected InvalidTransaction, got {:?}", other),
        }
    }

    #[test]
    fn test_deregister_validator() {
        let store = InMemoryStore::new();
        let addr = test_addr(12);

        register_validator(&store, test_validator(addr, 50)).unwrap();
        assert!(store.get_validator(&addr).unwrap().is_some());

        deregister_validator(&store, &addr).unwrap();
        assert!(store.get_validator(&addr).unwrap().is_none());
    }

    #[test]
    fn test_deregister_nonexistent_fails() {
        let store = InMemoryStore::new();
        let addr = test_addr(13);

        let result = deregister_validator(&store, &addr);
        assert!(result.is_err());
        match result {
            Err(CommpError::UnknownValidator(_)) => {}
            other => panic!("expected UnknownValidator, got {:?}", other),
        }
    }

    #[test]
    fn test_validator_update_contribution() {
        let store = InMemoryStore::new();
        let addr = test_addr(14);

        register_validator(&store, test_validator(addr, 50)).unwrap();

        // Verify initial value
        let got = store.get_validator(&addr).unwrap().unwrap();
        assert_eq!(got.capacity.contribution_percent, 50);

        // Update to 80%
        update_validator(&store, &addr, 80).unwrap();

        let got = store.get_validator(&addr).unwrap().unwrap();
        assert_eq!(got.capacity.contribution_percent, 80);
    }
}
