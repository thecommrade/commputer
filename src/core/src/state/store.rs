use std::collections::HashMap;
use std::sync::RwLock;

use serde::{Deserialize, Serialize};
use borsh::{BorshDeserialize, BorshSerialize};

use crate::block::{Block, BlockHash};
use crate::error::CommpError;
use crate::identity::{Address, ValidatorIdentity};

/// Minimal account record. Task 2 will replace this with the full AccountState.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct AccountRecord {
    pub balance: u64,
    pub nonce: u64,
}

/// Trait abstracting blockchain state storage.
///
/// Implementations must be thread-safe (`Send + Sync`). All methods return
/// `Result<T, CommpError>` so that persistent backends can surface I/O errors
/// through `CommpError::Storage`.
pub trait StateStore: Send + Sync {
    fn get_account(&self, addr: &Address) -> Result<Option<AccountRecord>, CommpError>;
    fn set_account(&self, addr: &Address, account: &AccountRecord) -> Result<(), CommpError>;
    fn all_accounts(&self) -> Result<Vec<(Address, AccountRecord)>, CommpError>;

    fn get_validator(&self, addr: &Address) -> Result<Option<ValidatorIdentity>, CommpError>;
    fn set_validator(&self, addr: &Address, identity: &ValidatorIdentity) -> Result<(), CommpError>;
    fn remove_validator(&self, addr: &Address) -> Result<(), CommpError>;
    fn validator_count(&self) -> Result<u64, CommpError>;
    fn all_validators(&self) -> Result<Vec<(Address, ValidatorIdentity)>, CommpError>;

    fn get_block_by_height(&self, height: u64) -> Result<Option<Block>, CommpError>;
    fn get_block_by_hash(&self, hash: &BlockHash) -> Result<Option<Block>, CommpError>;
    fn put_block(&self, block: &Block) -> Result<(), CommpError>;

    fn get_chain_tip(&self) -> Result<Option<(u64, BlockHash)>, CommpError>;
    fn set_chain_tip(&self, height: u64, hash: BlockHash) -> Result<(), CommpError>;
}

/// In-memory state store backed by `HashMap` behind `RwLock` for each collection.
/// Suitable for tests, short-lived nodes, and as a reference implementation.
pub struct InMemoryStore {
    accounts: RwLock<HashMap<Address, AccountRecord>>,
    validators: RwLock<HashMap<Address, ValidatorIdentity>>,
    blocks_by_height: RwLock<HashMap<u64, Block>>,
    blocks_by_hash: RwLock<HashMap<BlockHash, Block>>,
    chain_tip: RwLock<Option<(u64, BlockHash)>>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self {
            accounts: RwLock::new(HashMap::new()),
            validators: RwLock::new(HashMap::new()),
            blocks_by_height: RwLock::new(HashMap::new()),
            blocks_by_hash: RwLock::new(HashMap::new()),
            chain_tip: RwLock::new(None),
        }
    }
}

impl Default for InMemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl StateStore for InMemoryStore {
    fn get_account(&self, addr: &Address) -> Result<Option<AccountRecord>, CommpError> {
        let map = self.accounts.read()
            .map_err(|e| CommpError::Storage(format!("account lock poisoned: {}", e)))?;
        Ok(map.get(addr).cloned())
    }

    fn set_account(&self, addr: &Address, account: &AccountRecord) -> Result<(), CommpError> {
        let mut map = self.accounts.write()
            .map_err(|e| CommpError::Storage(format!("account lock poisoned: {}", e)))?;
        map.insert(*addr, account.clone());
        Ok(())
    }

    fn all_accounts(&self) -> Result<Vec<(Address, AccountRecord)>, CommpError> {
        let map = self.accounts.read()
            .map_err(|e| CommpError::Storage(format!("account lock poisoned: {}", e)))?;
        let mut accounts: Vec<(Address, AccountRecord)> = map.iter().map(|(k, v)| (*k, v.clone())).collect();
        accounts.sort_by_key(|(addr, _)| *addr);
        Ok(accounts)
    }

    fn get_validator(&self, addr: &Address) -> Result<Option<ValidatorIdentity>, CommpError> {
        let map = self.validators.read()
            .map_err(|e| CommpError::Storage(format!("validator lock poisoned: {}", e)))?;
        Ok(map.get(addr).cloned())
    }

    fn set_validator(&self, addr: &Address, identity: &ValidatorIdentity) -> Result<(), CommpError> {
        let mut map = self.validators.write()
            .map_err(|e| CommpError::Storage(format!("validator lock poisoned: {}", e)))?;
        map.insert(*addr, identity.clone());
        Ok(())
    }

    fn remove_validator(&self, addr: &Address) -> Result<(), CommpError> {
        let mut map = self.validators.write()
            .map_err(|e| CommpError::Storage(format!("validator lock poisoned: {}", e)))?;
        map.remove(addr);
        Ok(())
    }

    fn validator_count(&self) -> Result<u64, CommpError> {
        let map = self.validators.read()
            .map_err(|e| CommpError::Storage(format!("validator lock poisoned: {}", e)))?;
        Ok(map.len() as u64)
    }

    fn all_validators(&self) -> Result<Vec<(Address, ValidatorIdentity)>, CommpError> {
        let map = self.validators.read()
            .map_err(|e| CommpError::Storage(format!("validator lock poisoned: {}", e)))?;
        Ok(map.iter().map(|(k, v)| (*k, v.clone())).collect())
    }

    fn get_block_by_height(&self, height: u64) -> Result<Option<Block>, CommpError> {
        let map = self.blocks_by_height.read()
            .map_err(|e| CommpError::Storage(format!("block lock poisoned: {}", e)))?;
        Ok(map.get(&height).cloned())
    }

    fn get_block_by_hash(&self, hash: &BlockHash) -> Result<Option<Block>, CommpError> {
        let map = self.blocks_by_hash.read()
            .map_err(|e| CommpError::Storage(format!("block lock poisoned: {}", e)))?;
        Ok(map.get(hash).cloned())
    }

    fn put_block(&self, block: &Block) -> Result<(), CommpError> {
        let height = block.height();
        let hash = block.hash();
        {
            let mut map = self.blocks_by_height.write()
                .map_err(|e| CommpError::Storage(format!("block lock poisoned: {}", e)))?;
            map.insert(height, block.clone());
        }
        {
            let mut map = self.blocks_by_hash.write()
                .map_err(|e| CommpError::Storage(format!("block lock poisoned: {}", e)))?;
            map.insert(hash, block.clone());
        }
        Ok(())
    }

    fn get_chain_tip(&self) -> Result<Option<(u64, BlockHash)>, CommpError> {
        let tip = self.chain_tip.read()
            .map_err(|e| CommpError::Storage(format!("chain_tip lock poisoned: {}", e)))?;
        Ok(*tip)
    }

    fn set_chain_tip(&self, height: u64, hash: BlockHash) -> Result<(), CommpError> {
        let mut tip = self.chain_tip.write()
            .map_err(|e| CommpError::Storage(format!("chain_tip lock poisoned: {}", e)))?;
        *tip = Some((height, hash));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{test_addr, test_block};
    use crate::identity::{HardwareFingerprint, ResourceCapacity};
    use ed25519_dalek::SigningKey;

    fn make_validator(addr: Address) -> ValidatorIdentity {
        let signing_key = SigningKey::from_bytes(&addr.0);
        let public_key = signing_key.verifying_key();
        ValidatorIdentity {
            address: addr,
            public_key,
            hardware: HardwareFingerprint {
                cpu_model: "test-cpu".into(),
                cpu_cores: 4,
                ram_total_mb: 8192,
                gpu_model: None,
                gpu_vram_mb: None,
                storage_total_mb: 100_000,
                os_family: "linux".into(),
                network_speed_mbps: 1000,
            },
            capacity: ResourceCapacity {
                cpu_score: 100,
                gpu_score: 0,
                ram_available_mb: 4096,
                storage_available_mb: 50_000,
                bandwidth_kbps: 100_000,
                contribution_percent: 50,
            },
            registered_epoch: 0,
            cumulative_uptime_secs: 3600,
        }
    }

    #[test]
    fn test_account_roundtrip() {
        let store = InMemoryStore::new();
        let addr = test_addr(1);
        let record = AccountRecord { balance: 1000, nonce: 5 };

        store.set_account(&addr, &record).unwrap();
        let got = store.get_account(&addr).unwrap().expect("account should exist");
        assert_eq!(got.balance, 1000);
        assert_eq!(got.nonce, 5);
    }

    #[test]
    fn test_account_missing() {
        let store = InMemoryStore::new();
        let addr = test_addr(99);
        let got = store.get_account(&addr).unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn test_validator_roundtrip() {
        let store = InMemoryStore::new();
        let addr = test_addr(2);
        let vi = make_validator(addr);

        store.set_validator(&addr, &vi).unwrap();
        let got = store.get_validator(&addr).unwrap().expect("validator should exist");
        assert_eq!(got.address, addr);
        assert_eq!(got.registered_epoch, 0);
        assert_eq!(got.cumulative_uptime_secs, 3600);
    }

    #[test]
    fn test_validator_remove() {
        let store = InMemoryStore::new();
        let addr = test_addr(3);
        let vi = make_validator(addr);

        store.set_validator(&addr, &vi).unwrap();
        assert!(store.get_validator(&addr).unwrap().is_some());

        store.remove_validator(&addr).unwrap();
        assert!(store.get_validator(&addr).unwrap().is_none());
    }

    #[test]
    fn test_validator_count() {
        let store = InMemoryStore::new();
        assert_eq!(store.validator_count().unwrap(), 0);

        for i in 1..=3 {
            let addr = test_addr(i);
            store.set_validator(&addr, &make_validator(addr)).unwrap();
        }
        assert_eq!(store.validator_count().unwrap(), 3);
    }

    #[test]
    fn test_block_roundtrip() {
        let store = InMemoryStore::new();
        let block = test_block(42);
        let hash = block.hash();

        store.put_block(&block).unwrap();

        let by_height = store.get_block_by_height(42).unwrap().expect("block by height");
        assert_eq!(by_height.height(), 42);

        let by_hash = store.get_block_by_hash(&hash).unwrap().expect("block by hash");
        assert_eq!(by_hash.height(), 42);
        assert_eq!(by_hash.hash(), hash);
    }

    #[test]
    fn test_chain_tip() {
        let store = InMemoryStore::new();
        assert!(store.get_chain_tip().unwrap().is_none());

        let block = test_block(10);
        let hash = block.hash();
        store.set_chain_tip(10, hash).unwrap();

        let tip = store.get_chain_tip().unwrap().expect("tip should exist");
        assert_eq!(tip.0, 10);
        assert_eq!(tip.1, hash);
    }
}
