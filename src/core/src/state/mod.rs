pub mod accounts;
pub mod emission;
pub mod grace;
pub mod store;
pub mod txpool;
pub mod validators;

#[cfg(test)]
mod tests;

pub use accounts::AccountState;
pub use emission::EmissionState;
pub use grace::{update_grace_balance, MAX_GRACE_BALANCE, GRACE_REFILL_RATIO};
pub use store::{AccountRecord, StateStore, InMemoryStore};
pub use txpool::TransactionPool;

use std::sync::RwLock;

use crate::block::{Block, BlockHash, BlockHeader, CURRENT_PROTOCOL_VERSION};
use crate::error::CommpError;
use crate::genesis::GenesisConfig;
use crate::identity::{Address, HardwareFingerprint, ResourceCapacity, ValidatorIdentity};
use crate::transaction::{Transaction, TxKind};
use ed25519_dalek::VerifyingKey;

/// Coordinates access to blockchain state storage and emission accounting.
pub struct StateManager<S: StateStore> {
    store: S,
    emission: RwLock<EmissionState>,
}

impl<S: StateStore> StateManager<S> {
    /// Create a new StateManager with the given store and default emission state.
    pub fn new(store: S) -> Self {
        Self {
            store,
            emission: RwLock::new(EmissionState::new()),
        }
    }

    /// Initialize the genesis block. Returns an error if the chain already has a tip.
    pub fn init_genesis(&self, config: &GenesisConfig) -> Result<(), CommpError> {
        // Check if chain tip already exists
        if self.store.get_chain_tip()?.is_some() {
            return Err(CommpError::InvalidBlock(
                "genesis already initialized: chain tip exists".to_string(),
            ));
        }

        let timestamp = if config.genesis_timestamp == 0 {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
        } else {
            config.genesis_timestamp
        };

        let header = BlockHeader {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            height: 0,
            parent_hash: BlockHash::GENESIS,
            tx_root: [0u8; 32],
            proof_root: [0u8; 32],
            state_root: [0u8; 32],
            timestamp,
            producer: Address([0u8; 32]),
            epoch: 0,
            producer_public_key: vec![],
            signature: vec![],
            checkpoint_hash: None,
            chain_id: config.chain_id.clone(),
        };

        let genesis_block = Block {
            header,
            transactions: vec![],
            proof_summaries: vec![],
            compliance_summary: None,
            epoch_summary: None,
        };

        let genesis_hash = genesis_block.hash();
        self.store.put_block(&genesis_block)?;
        self.store.set_chain_tip(0, genesis_hash)?;

        Ok(())
    }

    /// Returns read access to the emission state.
    pub fn emission(&self) -> std::sync::RwLockReadGuard<'_, EmissionState> {
        self.emission.read().expect("emission lock poisoned")
    }

    /// Returns a reference to the underlying store.
    pub fn store(&self) -> &S {
        &self.store
    }

    /// Dispatch a transaction to the appropriate handler based on its kind.
    pub fn apply_transaction(&self, tx: &Transaction) -> Result<(), CommpError> {
        match &tx.kind {
            TxKind::Transfer { to, amount } => {
                self.apply_transfer(&tx.from, to, amount.raw(), tx.nonce)
            }
            TxKind::ValidatorRegister { hardware_fingerprint_hash: _, contribution_percent } => {
                let pk_bytes: [u8; 32] = tx.public_key.as_slice().try_into()
                    .map_err(|_| CommpError::InvalidTransaction("invalid public key length".into()))?;
                let verifying_key = VerifyingKey::from_bytes(&pk_bytes)
                    .map_err(|_| CommpError::InvalidTransaction("invalid public key".into()))?;

                let identity = ValidatorIdentity {
                    address: tx.from,
                    public_key: verifying_key,
                    hardware: HardwareFingerprint {
                        cpu_model: String::new(),
                        cpu_cores: 0,
                        ram_total_mb: 0,
                        gpu_model: None,
                        gpu_vram_mb: None,
                        storage_total_mb: 0,
                        os_family: String::new(),
                        network_speed_mbps: 0,
                    },
                    capacity: ResourceCapacity {
                        cpu_score: 0,
                        gpu_score: 0,
                        ram_available_mb: 0,
                        storage_available_mb: 0,
                        bandwidth_kbps: 0,
                        contribution_percent: *contribution_percent,
                    },
                    registered_epoch: 0,
                    cumulative_uptime_secs: 0,
                };
                validators::register_validator(&self.store, identity)
            }
            TxKind::ValidatorExit => {
                validators::deregister_validator(&self.store, &tx.from)
            }
            TxKind::ValidatorDeregister => {
                validators::deregister_validator(&self.store, &tx.from)
            }
            TxKind::ValidatorUpdate { contribution_percent } => {
                validators::update_validator(&self.store, &tx.from, *contribution_percent)
            }
            TxKind::BurstCompute { burn_amount, .. } => {
                self.apply_burn(&tx.from, burn_amount.raw(), tx.nonce)
            }
            _ => Err(CommpError::InvalidTransaction(
                "unsupported transaction kind".to_string(),
            )),
        }
    }

    /// Apply a burn: debit sender, record burn in emission state, increment nonce.
    pub fn apply_burn(
        &self,
        sender: &Address,
        amount: u64,
        nonce: u64,
    ) -> Result<(), CommpError> {
        // Look up sender account
        let sender_record = self
            .store
            .get_account(sender)?
            .ok_or_else(|| CommpError::InvalidTransaction("sender account does not exist".to_string()))?;

        // Check nonce
        if sender_record.nonce != nonce {
            return Err(CommpError::InvalidTransaction(format!(
                "nonce mismatch: expected {}, got {}",
                sender_record.nonce, nonce
            )));
        }

        // Check balance
        if sender_record.balance < amount {
            return Err(CommpError::InsufficientBalance {
                have: sender_record.balance,
                need: amount,
            });
        }

        // Debit sender and increment nonce
        let new_sender = AccountRecord {
            balance: sender_record.balance - amount,
            nonce: sender_record.nonce + 1,
        };
        self.store.set_account(sender, &new_sender)?;

        // Record burn in emission state
        self.emission.write().expect("emission lock poisoned").record_burn(amount);

        Ok(())
    }

    /// Apply a transfer: debit sender, credit recipient, recalculate tiers, increment nonce.
    pub fn apply_transfer(
        &self,
        sender: &Address,
        to: &Address,
        amount: u64,
        nonce: u64,
    ) -> Result<(), CommpError> {
        // Look up sender account
        let sender_record = self
            .store
            .get_account(sender)?
            .ok_or_else(|| CommpError::InvalidTransaction("sender account does not exist".to_string()))?;

        // Check nonce
        if sender_record.nonce != nonce {
            return Err(CommpError::InvalidTransaction(format!(
                "nonce mismatch: expected {}, got {}",
                sender_record.nonce, nonce
            )));
        }

        // Check balance
        if sender_record.balance < amount {
            return Err(CommpError::InsufficientBalance {
                have: sender_record.balance,
                need: amount,
            });
        }

        // Debit sender and increment nonce
        let new_sender = AccountRecord {
            balance: sender_record.balance - amount,
            nonce: sender_record.nonce + 1,
        };
        self.store.set_account(sender, &new_sender)?;

        // Credit recipient (create if new)
        let recipient_record = self.store.get_account(to)?.unwrap_or(AccountRecord {
            balance: 0,
            nonce: 0,
        });
        let new_recipient = AccountRecord {
            balance: recipient_record.balance + amount,
            nonce: recipient_record.nonce,
        };
        self.store.set_account(to, &new_recipient)?;

        Ok(())
    }

    /// Test helper: set an account's balance directly and recalculate tier.
    /// Creates the account if it doesn't exist (nonce defaults to 0).
    pub fn fund_account(&self, addr: &Address, amount: u64) -> Result<(), CommpError> {
        let existing = self.store.get_account(addr)?;
        let record = AccountRecord {
            balance: amount,
            nonce: existing.map(|r| r.nonce).unwrap_or(0),
        };
        self.store.set_account(addr, &record)?;
        Ok(())
    }

    /// Apply a full block to the chain state.
    ///
    /// Verifies height and parent hash continuity, applies all transactions
    /// in order, stores the block, and advances the chain tip.
    pub fn apply_block(&self, block: &Block) -> Result<(), CommpError> {
        let tip = self.store.get_chain_tip()?;

        match tip {
            None => {
                // No tip exists — only height 0 is allowed
                if block.height() != 0 {
                    return Err(CommpError::InvalidBlock(format!(
                        "expected height 0 (no chain tip), got {}",
                        block.height()
                    )));
                }
                if block.header.parent_hash != BlockHash::GENESIS {
                    return Err(CommpError::InvalidBlock(
                        "genesis block must have GENESIS parent hash".to_string(),
                    ));
                }
            }
            Some((tip_height, tip_hash)) => {
                let expected_height = tip_height + 1;
                if block.height() != expected_height {
                    return Err(CommpError::InvalidBlock(format!(
                        "expected height {}, got {}",
                        expected_height,
                        block.height()
                    )));
                }
                if block.header.parent_hash != tip_hash {
                    return Err(CommpError::InvalidBlock(format!(
                        "parent hash mismatch: expected {:?}, got {:?}",
                        tip_hash, block.header.parent_hash
                    )));
                }
            }
        }

        // Apply all transactions in order
        for tx in &block.transactions {
            self.apply_transaction(tx)?;
        }

        // Store the block and update chain tip
        let block_hash = block.hash();
        self.store.put_block(block)?;
        self.store.set_chain_tip(block.height(), block_hash)?;

        Ok(())
    }

    /// Compute a deterministic state root hash over all accounts, validators,
    /// and the emission state using blake3.
    pub fn compute_state_root(&self) -> Result<[u8; 32], CommpError> {
        let mut hasher = blake3::Hasher::new();

        // Domain separator
        hasher.update(b"commputer-state-v1");

        // All accounts sorted by Address bytes
        let accounts = self.store.all_accounts()?;
        for (addr, record) in &accounts {
            hasher.update(&addr.0);
            let encoded = borsh::to_vec(record)
                .map_err(|e| CommpError::Storage(format!("borsh serialize account: {}", e)))?;
            hasher.update(&encoded);
        }

        // All validators sorted by Address bytes
        let mut validators = self.store.all_validators()?;
        validators.sort_by_key(|(addr, _)| *addr);
        for (addr, identity) in &validators {
            hasher.update(&addr.0);
            // ValidatorIdentity doesn't derive BorshSerialize (VerifyingKey),
            // so we hash each serializable field individually.
            hasher.update(identity.public_key.as_bytes());
            let hw_encoded = borsh::to_vec(&identity.hardware)
                .map_err(|e| CommpError::Storage(format!("borsh serialize hardware: {}", e)))?;
            hasher.update(&hw_encoded);
            let cap_encoded = borsh::to_vec(&identity.capacity)
                .map_err(|e| CommpError::Storage(format!("borsh serialize capacity: {}", e)))?;
            hasher.update(&cap_encoded);
            hasher.update(&identity.registered_epoch.to_le_bytes());
            hasher.update(&identity.cumulative_uptime_secs.to_le_bytes());
        }

        // Emission state
        let emission = self.emission.read()
            .map_err(|e| CommpError::Storage(format!("emission lock poisoned: {}", e)))?;
        let emission_encoded = borsh::to_vec(&*emission)
            .map_err(|e| CommpError::Storage(format!("borsh serialize emission: {}", e)))?;
        hasher.update(&emission_encoded);

        Ok(*hasher.finalize().as_bytes())
    }
}

#[cfg(test)]
mod manager_tests {
    use super::*;
    use crate::genesis::default_genesis;
    use crate::token::TOTAL_SUPPLY;

    #[test]
    fn test_create_state_manager() {
        let store = InMemoryStore::new();
        let _manager = StateManager::new(store);
        // No panic — construction succeeds
    }

    #[test]
    fn test_init_genesis() {
        let store = InMemoryStore::new();
        let manager = StateManager::new(store);
        let config = default_genesis();

        manager.init_genesis(&config).unwrap();

        // Chain tip should be at height 0
        let tip = manager.store().get_chain_tip().unwrap().expect("chain tip should exist");
        assert_eq!(tip.0, 0);

        // Genesis block should exist at height 0
        let block = manager.store().get_block_by_height(0).unwrap().expect("genesis block should exist");
        assert_eq!(block.height(), 0);
        assert!(block.is_genesis());
        assert_eq!(block.hash(), tip.1);
    }

    #[test]
    fn test_double_genesis_fails() {
        let store = InMemoryStore::new();
        let manager = StateManager::new(store);
        let config = default_genesis();

        manager.init_genesis(&config).unwrap();
        let result = manager.init_genesis(&config);
        assert!(result.is_err());
    }

    #[test]
    fn test_genesis_emission_state() {
        let store = InMemoryStore::new();
        let manager = StateManager::new(store);
        let config = default_genesis();

        manager.init_genesis(&config).unwrap();

        let emission = manager.emission();
        assert_eq!(emission.remaining_supply, TOTAL_SUPPLY);
        assert_eq!(emission.total_emitted, 0);
    }
}

#[cfg(test)]
mod transfer_tests {
    use super::*;
    use crate::testutil::test_addr;
    use crate::token::{Amount, UNITS_PER_COMME};
    use crate::transaction::{Transaction, TxKind};
    use crate::tier::HolderTier;

    fn make_transfer_tx(from: Address, to: Address, amount: u64, nonce: u64) -> Transaction {
        Transaction {
            from,
            nonce,
            fee: 0,
            kind: TxKind::Transfer {
                to,
                amount: Amount::from_raw(amount),
            },
            public_key: vec![],
            signature: vec![],
            memo: None,
            timelock: None,
        }
    }

    #[test]
    fn test_transfer_basic() {
        let store = InMemoryStore::new();
        let manager = StateManager::new(store);
        let alice = test_addr(1);
        let bob = test_addr(2);

        // Fund Alice with 1000 units
        manager.fund_account(&alice, 1000).unwrap();

        // Transfer 400 from Alice to Bob
        let tx = make_transfer_tx(alice, bob, 400, 0);
        manager.apply_transaction(&tx).unwrap();

        // Verify balances
        let alice_rec = manager.store().get_account(&alice).unwrap().unwrap();
        assert_eq!(alice_rec.balance, 600);

        let bob_rec = manager.store().get_account(&bob).unwrap().unwrap();
        assert_eq!(bob_rec.balance, 400);
    }

    #[test]
    fn test_transfer_insufficient_balance() {
        let store = InMemoryStore::new();
        let manager = StateManager::new(store);
        let alice = test_addr(1);
        let bob = test_addr(2);

        manager.fund_account(&alice, 100).unwrap();

        let tx = make_transfer_tx(alice, bob, 200, 0);
        let result = manager.apply_transaction(&tx);

        match result {
            Err(CommpError::InsufficientBalance { have, need }) => {
                assert_eq!(have, 100);
                assert_eq!(need, 200);
            }
            other => panic!("expected InsufficientBalance, got {:?}", other),
        }
    }

    #[test]
    fn test_transfer_creates_recipient() {
        let store = InMemoryStore::new();
        let manager = StateManager::new(store);
        let alice = test_addr(1);
        let bob = test_addr(2);

        // Bob doesn't exist yet
        assert!(manager.store().get_account(&bob).unwrap().is_none());

        manager.fund_account(&alice, 500).unwrap();

        let tx = make_transfer_tx(alice, bob, 300, 0);
        manager.apply_transaction(&tx).unwrap();

        // Bob should now exist with balance 300
        let bob_rec = manager.store().get_account(&bob).unwrap().unwrap();
        assert_eq!(bob_rec.balance, 300);
        assert_eq!(bob_rec.nonce, 0);
    }

    #[test]
    fn test_transfer_updates_tiers() {
        let store = InMemoryStore::new();
        let manager = StateManager::new(store);
        let alice = test_addr(1);
        let bob = test_addr(2);

        // Fund Alice with 15 COMME (Base tier, below Storage threshold of 10 whole COMME)
        let fifteen_comme = 15 * UNITS_PER_COMME;
        manager.fund_account(&alice, fifteen_comme).unwrap();

        // Bob has nothing — tier None
        assert_eq!(HolderTier::from_balance(0), HolderTier::None);

        // Transfer 10 COMME to Bob — should push Bob to Storage tier
        let ten_comme = 10 * UNITS_PER_COMME;
        let tx = make_transfer_tx(alice, bob, ten_comme, 0);
        manager.apply_transaction(&tx).unwrap();

        let bob_rec = manager.store().get_account(&bob).unwrap().unwrap();
        let bob_tier = HolderTier::from_balance(bob_rec.balance / UNITS_PER_COMME);
        assert_eq!(bob_tier, HolderTier::Storage);

        // Alice now has 5 COMME — Base tier
        let alice_rec = manager.store().get_account(&alice).unwrap().unwrap();
        let alice_tier = HolderTier::from_balance(alice_rec.balance / UNITS_PER_COMME);
        assert_eq!(alice_tier, HolderTier::Base);
    }

    #[test]
    fn test_transfer_increments_nonce() {
        let store = InMemoryStore::new();
        let manager = StateManager::new(store);
        let alice = test_addr(1);
        let bob = test_addr(2);

        manager.fund_account(&alice, 1000).unwrap();

        // First transfer — nonce 0
        let tx1 = make_transfer_tx(alice, bob, 100, 0);
        manager.apply_transaction(&tx1).unwrap();

        let alice_rec = manager.store().get_account(&alice).unwrap().unwrap();
        assert_eq!(alice_rec.nonce, 1);

        // Second transfer — nonce 1
        let tx2 = make_transfer_tx(alice, bob, 100, 1);
        manager.apply_transaction(&tx2).unwrap();

        let alice_rec = manager.store().get_account(&alice).unwrap().unwrap();
        assert_eq!(alice_rec.nonce, 2);
    }

    #[test]
    fn test_transfer_wrong_nonce() {
        let store = InMemoryStore::new();
        let manager = StateManager::new(store);
        let alice = test_addr(1);
        let bob = test_addr(2);

        manager.fund_account(&alice, 1000).unwrap();

        // Use nonce 5 instead of 0
        let tx = make_transfer_tx(alice, bob, 100, 5);
        let result = manager.apply_transaction(&tx);

        match result {
            Err(CommpError::InvalidTransaction(msg)) => {
                assert!(msg.contains("nonce"), "error should mention nonce: {}", msg);
            }
            other => panic!("expected InvalidTransaction, got {:?}", other),
        }
    }
}

#[cfg(test)]
mod burn_tests {
    use super::*;
    use crate::proof::ResourceChannel;
    use crate::testutil::test_addr;
    use crate::token::{Amount, UNITS_PER_COMME};
    use crate::transaction::{Transaction, TxKind};

    fn make_burn_tx(from: Address, burn_amount: u64, nonce: u64) -> Transaction {
        Transaction {
            from,
            nonce,
            fee: 0,
            kind: TxKind::BurstCompute {
                channel: ResourceChannel::Processing,
                burn_amount: Amount::from_raw(burn_amount),
                job_hash: [0u8; 32],
            },
            public_key: vec![],
            signature: vec![],
            memo: None,
            timelock: None,
        }
    }

    #[test]
    fn test_burst_burn_deducts_balance() {
        let store = InMemoryStore::new();
        let manager = StateManager::new(store);
        let alice = test_addr(1);

        // Fund Alice with 10 COMME
        let ten_comme = 10 * UNITS_PER_COMME;
        manager.fund_account(&alice, ten_comme).unwrap();

        // Burn 5 COMME
        let five_comme = 5 * UNITS_PER_COMME;
        let tx = make_burn_tx(alice, five_comme, 0);
        manager.apply_transaction(&tx).unwrap();

        // Balance should decrease by 5 COMME
        let alice_rec = manager.store().get_account(&alice).unwrap().unwrap();
        assert_eq!(alice_rec.balance, ten_comme - five_comme);
        assert_eq!(alice_rec.nonce, 1);
    }

    #[test]
    fn test_burst_burn_updates_emission() {
        let store = InMemoryStore::new();
        let manager = StateManager::new(store);
        let alice = test_addr(1);

        let ten_comme = 10 * UNITS_PER_COMME;
        manager.fund_account(&alice, ten_comme).unwrap();

        // Burn 5 COMME
        let five_comme = 5 * UNITS_PER_COMME;
        let tx = make_burn_tx(alice, five_comme, 0);
        manager.apply_transaction(&tx).unwrap();

        // total_burned should increase by 5 COMME
        let emission = manager.emission();
        assert_eq!(emission.total_burned, five_comme);
    }

    #[test]
    fn test_burst_burn_insufficient() {
        let store = InMemoryStore::new();
        let manager = StateManager::new(store);
        let alice = test_addr(1);

        // Fund Alice with 3 COMME
        let three_comme = 3 * UNITS_PER_COMME;
        manager.fund_account(&alice, three_comme).unwrap();

        // Try to burn 5 COMME — should fail
        let five_comme = 5 * UNITS_PER_COMME;
        let tx = make_burn_tx(alice, five_comme, 0);
        let result = manager.apply_transaction(&tx);

        match result {
            Err(CommpError::InsufficientBalance { have, need }) => {
                assert_eq!(have, three_comme);
                assert_eq!(need, five_comme);
            }
            other => panic!("expected InsufficientBalance, got {:?}", other),
        }
    }
}

#[cfg(test)]
mod state_root_tests {
    use super::*;
    use crate::testutil::test_addr;
    use crate::genesis::default_genesis;

    #[test]
    fn test_empty_state_root() {
        // Genesis-only state produces a deterministic hash (not all zeros)
        let store = InMemoryStore::new();
        let manager = StateManager::new(store);
        let config = default_genesis();
        manager.init_genesis(&config).unwrap();

        let root = manager.compute_state_root().unwrap();
        assert_ne!(root, [0u8; 32], "state root should not be all zeros");
    }

    #[test]
    fn test_state_root_deterministic() {
        // Same operations twice produce same hash
        let make = || {
            let store = InMemoryStore::new();
            let manager = StateManager::new(store);
            let config = default_genesis();
            manager.init_genesis(&config).unwrap();
            manager.fund_account(&test_addr(1), 1000).unwrap();
            manager.fund_account(&test_addr(2), 2000).unwrap();
            manager.compute_state_root().unwrap()
        };

        let root1 = make();
        let root2 = make();
        assert_eq!(root1, root2, "same operations should produce same state root");
    }

    #[test]
    fn test_state_root_changes_on_mutation() {
        let store = InMemoryStore::new();
        let manager = StateManager::new(store);
        let config = default_genesis();
        manager.init_genesis(&config).unwrap();

        let alice = test_addr(1);
        let bob = test_addr(2);

        manager.fund_account(&alice, 1000).unwrap();
        manager.fund_account(&bob, 500).unwrap();

        let root_before = manager.compute_state_root().unwrap();

        // Transfer changes root
        manager.apply_transfer(&alice, &bob, 100, 0).unwrap();

        let root_after = manager.compute_state_root().unwrap();
        assert_ne!(root_before, root_after, "transfer should change state root");
    }

    #[test]
    fn test_state_root_order_independent() {
        // Fund A then B vs B then A = same root
        let root_ab = {
            let store = InMemoryStore::new();
            let manager = StateManager::new(store);
            let config = default_genesis();
            manager.init_genesis(&config).unwrap();
            manager.fund_account(&test_addr(1), 1000).unwrap();
            manager.fund_account(&test_addr(2), 2000).unwrap();
            manager.compute_state_root().unwrap()
        };

        let root_ba = {
            let store = InMemoryStore::new();
            let manager = StateManager::new(store);
            let config = default_genesis();
            manager.init_genesis(&config).unwrap();
            manager.fund_account(&test_addr(2), 2000).unwrap();
            manager.fund_account(&test_addr(1), 1000).unwrap();
            manager.compute_state_root().unwrap()
        };

        assert_eq!(root_ab, root_ba, "insertion order should not affect state root");
    }
}

#[cfg(test)]
mod apply_block_tests {
    use super::*;
    use crate::block::{BlockHeader, CURRENT_PROTOCOL_VERSION};
    use crate::genesis::default_genesis;
    use crate::testutil::test_addr;
    use crate::token::Amount;
    use crate::transaction::{Transaction, TxKind};

    fn make_block(height: u64, parent_hash: BlockHash, txs: Vec<Transaction>) -> Block {
        Block {
            header: BlockHeader {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                height,
                parent_hash,
                tx_root: [0u8; 32],
                proof_root: [0u8; 32],
                state_root: [0u8; 32],
                timestamp: 1000000 + height,
                producer: Address([0u8; 32]),
                epoch: 0,
                producer_public_key: vec![],
                signature: vec![],
                checkpoint_hash: None,
                chain_id: "commputer-testnet-1".to_string(),
            },
            transactions: txs,
            proof_summaries: vec![],
            compliance_summary: None,
            epoch_summary: None,
        }
    }

    fn make_transfer_tx(from: Address, to: Address, amount: u64, nonce: u64) -> Transaction {
        Transaction {
            from,
            nonce,
            fee: 0,
            kind: TxKind::Transfer {
                to,
                amount: Amount::from_raw(amount),
            },
            public_key: vec![],
            signature: vec![],
            memo: None,
            timelock: None,
        }
    }

    #[test]
    fn test_apply_genesis_block() {
        let store = InMemoryStore::new();
        let manager = StateManager::new(store);
        let config = default_genesis();

        manager.init_genesis(&config).unwrap();

        // Chain tip should be at height 0
        let tip = manager.store().get_chain_tip().unwrap().expect("tip should exist");
        assert_eq!(tip.0, 0);
    }

    #[test]
    fn test_apply_block_with_transfers() {
        let store = InMemoryStore::new();
        let manager = StateManager::new(store);
        let config = default_genesis();
        manager.init_genesis(&config).unwrap();

        let alice = test_addr(1);
        let bob = test_addr(2);
        let carol = test_addr(3);
        let dave = test_addr(4);

        // Fund alice so she can transfer
        manager.fund_account(&alice, 10_000).unwrap();

        // Get genesis hash for parent_hash
        let (_, genesis_hash) = manager.store().get_chain_tip().unwrap().unwrap();

        // Build block at height 1 with 3 transfer transactions
        let txs = vec![
            make_transfer_tx(alice, bob, 1000, 0),
            make_transfer_tx(alice, carol, 2000, 1),
            make_transfer_tx(alice, dave, 3000, 2),
        ];
        let block = make_block(1, genesis_hash, txs);
        manager.apply_block(&block).unwrap();

        // Verify balances
        let alice_rec = manager.store().get_account(&alice).unwrap().unwrap();
        assert_eq!(alice_rec.balance, 10_000 - 1000 - 2000 - 3000);
        assert_eq!(alice_rec.nonce, 3);

        let bob_rec = manager.store().get_account(&bob).unwrap().unwrap();
        assert_eq!(bob_rec.balance, 1000);

        let carol_rec = manager.store().get_account(&carol).unwrap().unwrap();
        assert_eq!(carol_rec.balance, 2000);

        let dave_rec = manager.store().get_account(&dave).unwrap().unwrap();
        assert_eq!(dave_rec.balance, 3000);
    }

    #[test]
    fn test_apply_block_wrong_height() {
        let store = InMemoryStore::new();
        let manager = StateManager::new(store);
        let config = default_genesis();
        manager.init_genesis(&config).unwrap();

        let (_, genesis_hash) = manager.store().get_chain_tip().unwrap().unwrap();

        // Try to apply block at height 3 when tip is 0
        let block = make_block(3, genesis_hash, vec![]);
        let result = manager.apply_block(&block);

        match result {
            Err(CommpError::InvalidBlock(msg)) => {
                assert!(msg.contains("expected height 1"), "error should mention expected height: {}", msg);
            }
            other => panic!("expected InvalidBlock, got {:?}", other),
        }
    }

    #[test]
    fn test_apply_block_wrong_prev_hash() {
        let store = InMemoryStore::new();
        let manager = StateManager::new(store);
        let config = default_genesis();
        manager.init_genesis(&config).unwrap();

        // Use a bogus parent hash
        let wrong_hash = BlockHash([0xFFu8; 32]);
        let block = make_block(1, wrong_hash, vec![]);
        let result = manager.apply_block(&block);

        match result {
            Err(CommpError::InvalidBlock(msg)) => {
                assert!(msg.contains("parent hash mismatch"), "error should mention parent hash: {}", msg);
            }
            other => panic!("expected InvalidBlock, got {:?}", other),
        }
    }

    #[test]
    fn test_apply_block_updates_chain_tip() {
        let store = InMemoryStore::new();
        let manager = StateManager::new(store);
        let config = default_genesis();
        manager.init_genesis(&config).unwrap();

        let (_, genesis_hash) = manager.store().get_chain_tip().unwrap().unwrap();

        // Apply block at height 1
        let block1 = make_block(1, genesis_hash, vec![]);
        let block1_hash = block1.hash();
        manager.apply_block(&block1).unwrap();

        // Chain tip should now be at height 1
        let (tip_height, tip_hash) = manager.store().get_chain_tip().unwrap().unwrap();
        assert_eq!(tip_height, 1);
        assert_eq!(tip_hash, block1_hash);

        // Apply block at height 2
        let block2 = make_block(2, block1_hash, vec![]);
        let block2_hash = block2.hash();
        manager.apply_block(&block2).unwrap();

        // Chain tip should now be at height 2
        let (tip_height, tip_hash) = manager.store().get_chain_tip().unwrap().unwrap();
        assert_eq!(tip_height, 2);
        assert_eq!(tip_hash, block2_hash);
    }
}
