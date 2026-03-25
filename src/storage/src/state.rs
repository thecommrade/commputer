use std::path::Path;
use commputer_core::block::Block;
use commputer_core::token::TOTAL_SUPPLY;
use commputer_core::transaction::{TxKind, Transaction};
use commputer_core::compliance::NerfRate;
use tracing::{info, warn};
use crate::account::AccountStore;
use crate::blockstore::BlockStore;
use crate::receipt::{AccountHistoryIndex, ReceiptStore, TxReceipt};
use crate::rocks::{self, RocksStore};

/// Feature 122: Finality depth — blocks older than this many confirmations cannot be reorged.
pub const FINALITY_DEPTH: u64 = 10;

/// Feature 135: Checkpoint interval — every N blocks is a checkpoint that cannot be reorged past.
pub const CHECKPOINT_INTERVAL: u64 = 100;

/// The full chain state — accounts, blocks, supply tracking.
/// Optionally backed by RocksDB for persistence across restarts.
pub struct ChainState {
    pub accounts: AccountStore,
    pub blocks: BlockStore,
    /// Total $COMME emitted so far (in raw units).
    pub total_emitted: u64,
    /// Total $COMME burned so far (in raw units).
    pub total_burned: u64,
    /// Current network-wide nerf rate.
    pub nerf_rate: NerfRate,
    /// Current epoch number.
    pub current_epoch: u64,
    /// Transaction receipt store.
    pub receipts: ReceiptStore,
    /// Address -> tx hash reverse index.
    pub history: AccountHistoryIndex,
    /// Feature 134: Cumulative CRS (composite resource score) for fork choice.
    pub cumulative_score: u64,
    /// Optional RocksDB persistent layer. None = in-memory only (tests).
    rocks: Option<RocksStore>,
}

// Manual Debug impl since RocksStore doesn't derive Debug.
impl std::fmt::Debug for ChainState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChainState")
            .field("accounts", &self.accounts)
            .field("blocks", &self.blocks)
            .field("total_emitted", &self.total_emitted)
            .field("total_burned", &self.total_burned)
            .field("nerf_rate", &self.nerf_rate)
            .field("current_epoch", &self.current_epoch)
            .field("cumulative_score", &self.cumulative_score)
            .field("persistent", &self.rocks.is_some())
            .finish()
    }
}

impl ChainState {
    /// Number of recent blocks to keep in memory when RocksDB is enabled.
    /// Older blocks are pruned from the in-memory BlockStore but remain in RocksDB.
    pub const MEMORY_BLOCK_RETENTION: u64 = 1000;

    /// Create a new in-memory-only ChainState. Existing behavior, tests unchanged.
    pub fn new() -> Self {
        Self {
            accounts: AccountStore::new(),
            blocks: BlockStore::new(),
            total_emitted: 0,
            total_burned: 0,
            nerf_rate: NerfRate::INITIAL,
            current_epoch: 0,
            receipts: ReceiptStore::new(),
            history: AccountHistoryIndex::new(),
            cumulative_score: 0,
            rocks: None,
        }
    }

    /// Open a persistent ChainState backed by RocksDB at the given path.
    /// Loads all state from disk into the in-memory stores.
    pub fn open(path: &Path) -> Result<Self, StateError> {
        let rocks = RocksStore::open(path)
            .map_err(|e| StateError::StorageError(e.to_string()))?;

        // Load meta counters.
        let total_emitted = rocks
            .get_meta_u64(rocks::META_TOTAL_EMITTED)
            .map_err(|e| StateError::StorageError(e.to_string()))?
            .unwrap_or(0);
        let total_burned = rocks
            .get_meta_u64(rocks::META_TOTAL_BURNED)
            .map_err(|e| StateError::StorageError(e.to_string()))?
            .unwrap_or(0);
        let current_epoch = rocks
            .get_meta_u64(rocks::META_CURRENT_EPOCH)
            .map_err(|e| StateError::StorageError(e.to_string()))?
            .unwrap_or(0);
        let nerf_rate_bps = rocks
            .get_meta_u64(rocks::META_NERF_RATE_BPS)
            .map_err(|e| StateError::StorageError(e.to_string()))?
            .unwrap_or(8000) as u32;

        // Load all accounts into the in-memory store.
        let mut accounts = AccountStore::new();
        for account in rocks.all_accounts() {
            accounts.put(account);
        }

        // Load all blocks into the in-memory store.
        let mut blocks = BlockStore::new();
        for block in rocks.all_blocks_by_height() {
            blocks.put(block);
        }

        let account_count = accounts.len();
        let block_count = blocks.len();
        let height = blocks.height();

        info!(
            "Loaded state from disk: {} blocks (height {}), {} accounts, epoch {}",
            block_count, height, account_count, current_epoch,
        );

        Ok(Self {
            accounts,
            blocks,
            total_emitted,
            total_burned,
            nerf_rate: NerfRate { rate_bps: nerf_rate_bps },
            current_epoch,
            receipts: ReceiptStore::new(),
            history: AccountHistoryIndex::new(),
            cumulative_score: 0,
            rocks: Some(rocks),
        })
    }

    /// Flush the full current state to RocksDB. Call after applying blocks or
    /// modifying accounts directly (e.g., funding via emission).
    pub fn flush(&self) -> Result<(), StateError> {
        if let Some(ref rocks) = self.rocks {
            self.flush_to_rocks(rocks)?;
        }
        Ok(())
    }

    /// Retrieve a block by height. Checks in-memory first, falls back to RocksDB.
    pub fn get_block_by_height(&self, height: u64) -> Option<Block> {
        // Try in-memory first.
        if let Some(block) = self.blocks.get_by_height(height) {
            return Some(block.clone());
        }
        // Fall back to RocksDB for pruned blocks.
        if let Some(ref rocks) = self.rocks {
            if let Ok(Some(block)) = rocks.get_block_by_height(height) {
                return Some(block);
            }
        }
        None
    }

    /// Serialize the full state to a JSON snapshot.
    pub fn snapshot(&self) -> serde_json::Value {
        let accounts: Vec<serde_json::Value> = self.accounts.iter().map(|a| {
            serde_json::json!({
                "address": hex::encode(a.address.0),
                "balance": a.balance.raw(),
                "nonce": a.nonce,
                "is_validator": a.is_validator,
                "total_mined": a.total_mined.raw(),
                "total_burned": a.total_burned.raw(),
                "grace_balance_secs": a.grace_balance_secs,
                "cumulative_uptime_secs": a.cumulative_uptime_secs,
            })
        }).collect();

        serde_json::json!({
            "height": self.blocks.height(),
            "total_emitted": self.total_emitted,
            "total_burned": self.total_burned,
            "current_epoch": self.current_epoch,
            "state_root": hex::encode(self.compute_state_root()),
            "accounts": accounts,
        })
    }

    /// Save a state snapshot to a file.
    pub fn save_snapshot(&self, path: &std::path::Path) -> Result<(), StateError> {
        let snap = self.snapshot();
        let json = serde_json::to_string_pretty(&snap)
            .map_err(|e| StateError::StorageError(e.to_string()))?;
        std::fs::write(path, json)
            .map_err(|e| StateError::StorageError(e.to_string()))?;
        info!("State snapshot saved to {} at height {}", path.display(), self.blocks.height());
        Ok(())
    }

    /// Compute the state root from the current account store.
    pub fn compute_state_root(&self) -> [u8; 32] {
        self.accounts.compute_state_root()
    }

    /// Remaining supply available for emission.
    pub fn remaining_supply(&self) -> u64 {
        TOTAL_SUPPLY.saturating_sub(self.total_emitted)
    }

    /// Circulating supply (emitted minus burned).
    pub fn circulating_supply(&self) -> u64 {
        self.total_emitted.saturating_sub(self.total_burned)
    }

    /// Whether emergency access mode is active (supply below 1M COMME).
    /// Only triggers after emission has begun — not at genesis.
    pub fn is_emergency_access(&self) -> bool {
        if self.total_emitted == 0 {
            return false;
        }
        let circulating_comme = self.circulating_supply() / commputer_core::token::UNITS_PER_COMME;
        circulating_comme < commputer_core::tier::HolderTier::EMERGENCY_SUPPLY_THRESHOLD
    }

    /// Apply a block to the chain state.
    /// Processes all transactions, updates balances, records burns.
    pub fn apply_block(&mut self, block: &Block) -> Result<(), StateError> {
        // Verify block connects to current chain.
        if block.height() > 0 {
            let expected_height = self.blocks.height() + 1;
            if block.height() != expected_height {
                return Err(StateError::InvalidHeight {
                    expected: expected_height,
                    got: block.height(),
                });
            }
        }

        // Process transactions.
        for tx in &block.transactions {
            self.apply_transaction(tx)?;
        }

        // Store block.
        self.blocks.put(block.clone());

        // Persist to RocksDB if enabled.
        if let Some(ref rocks) = self.rocks {
            rocks.put_block(block)
                .map_err(|e| StateError::StorageError(e.to_string()))?;
            self.flush_meta(rocks)?;
            // Prune old blocks from memory (they remain in RocksDB).
            self.blocks.prune(Self::MEMORY_BLOCK_RETENTION);
        }

        Ok(())
    }

    /// Apply a block to the chain state, first verifying that all transactions
    /// have a structurally valid 64-byte signature. Use this for blocks received
    /// from the network. The original `apply_block` remains for genesis and tests.
    pub fn apply_block_validated(&mut self, block: &Block) -> Result<(), StateError> {
        // Verify block height.
        if block.height() > 0 {
            let expected = self.blocks.height() + 1;
            if block.height() != expected {
                return Err(StateError::InvalidHeight { expected, got: block.height() });
            }
        }

        // Verify parent hash matches (except genesis).
        if block.height() > 0 {
            if let Some(latest) = self.blocks.latest() {
                if block.header.parent_hash != latest.hash() {
                    return Err(StateError::InvalidBlock("parent hash mismatch".into()));
                }
            }
        }

        // Verify merkle roots match block contents.
        if !block.verify_roots() {
            return Err(StateError::InvalidBlock("merkle root mismatch".into()));
        }

        // Cryptographically verify all transaction signatures.
        for tx in &block.transactions {
            if !tx.verify() {
                return Err(StateError::InvalidSignature(
                    format!("transaction from {:?} failed signature verification", tx.from)
                ));
            }
        }

        // Process transactions and generate receipts.
        let block_hash = block.hash();
        for (i, tx) in block.transactions.iter().enumerate() {
            self.apply_transaction(tx)?;
            let tx_hash = tx.hash();
            self.receipts.insert(TxReceipt {
                tx_hash,
                block_hash,
                block_height: block.height(),
                tx_index: i,
                success: true,
            });
            // Record in address history index.
            self.history.record(tx.from, tx_hash);
            if let commputer_core::transaction::TxKind::Transfer { to, .. } = &tx.kind {
                self.history.record(*to, tx_hash);
            }
        }

        // Store block.
        self.blocks.put(block.clone());

        // Persist to RocksDB if enabled.
        if let Some(ref rocks) = self.rocks {
            rocks.put_block(block)
                .map_err(|e| StateError::StorageError(e.to_string()))?;
            self.flush_meta(rocks)?;
            // Prune old blocks from memory (they remain in RocksDB).
            self.blocks.prune(Self::MEMORY_BLOCK_RETENTION);
        }

        Ok(())
    }

    /// Apply a single transaction to the state.
    fn apply_transaction(
        &mut self,
        tx: &commputer_core::transaction::Transaction,
    ) -> Result<(), StateError> {
        let sender = self.accounts.get_or_create(tx.from);

        // Verify nonce.
        if tx.nonce != sender.nonce {
            return Err(StateError::InvalidNonce {
                expected: sender.nonce,
                got: tx.nonce,
            });
        }

        // Deduct and burn fee.
        if tx.fee > 0 {
            let fee_amount = commputer_core::token::Amount::from_raw(tx.fee);
            if sender.balance.raw() < tx.fee {
                return Err(StateError::InsufficientBalance);
            }
            sender.balance = sender.balance.checked_sub(fee_amount)
                .ok_or(StateError::InsufficientBalance)?;
            self.total_burned = self.total_burned.saturating_add(tx.fee);
        }

        match &tx.kind {
            TxKind::Transfer { to, amount } => {
                let sender_balance = sender.balance;
                if sender_balance.raw() < amount.raw() {
                    return Err(StateError::InsufficientBalance);
                }
                let old_tier = sender.tier();
                sender.balance = sender_balance.checked_sub(*amount)
                    .ok_or(StateError::InsufficientBalance)?;
                sender.nonce += 1;
                let new_tier = sender.tier();
                if old_tier != new_tier {
                    info!("Tier change: {} went from {:?} to {:?}", tx.from, old_tier, new_tier);
                }

                let recipient = self.accounts.get_or_create(*to);
                let old_recv_tier = recipient.tier();
                recipient.balance = recipient.balance.checked_add(*amount)
                    .ok_or(StateError::Overflow)?;
                let new_recv_tier = recipient.tier();
                if old_recv_tier != new_recv_tier {
                    info!("Tier change: {} went from {:?} to {:?}", to, old_recv_tier, new_recv_tier);
                }
            }

            TxKind::ValidatorRegister { .. } => {
                sender.is_validator = true;
                sender.nonce += 1;
            }

            TxKind::ValidatorExit => {
                sender.is_validator = false;
                sender.nonce += 1;
            }

            TxKind::BurstCompute { burn_amount, .. } => {
                let sender_balance = sender.balance;
                if sender_balance.raw() < burn_amount.raw() {
                    return Err(StateError::InsufficientBalance);
                }
                sender.balance = sender_balance.checked_sub(*burn_amount)
                    .ok_or(StateError::InsufficientBalance)?;
                sender.total_burned = sender.total_burned.checked_add(*burn_amount)
                    .ok_or(StateError::Overflow)?;
                sender.nonce += 1;
                self.total_burned = self.total_burned.saturating_add(burn_amount.raw());
            }

            TxKind::MilestoneBurn { burn_amount, .. } => {
                self.total_burned = self.total_burned.saturating_add(burn_amount.raw());
            }

            TxKind::CharitableDonation { burn_amount, .. } => {
                self.total_burned = self.total_burned.saturating_add(burn_amount.raw());
            }

            TxKind::StorageWill { contact_hashes, .. } => {
                sender.will_contacts = contact_hashes.clone();
                sender.nonce += 1;
            }

            TxKind::ValidatorUpdate { .. } => {
                sender.nonce += 1;
            }

            TxKind::CharitableVote { .. } => {
                sender.nonce += 1;
            }
        }

        Ok(())
    }

    /// Record emission for an epoch (mining rewards distributed to validators).
    pub fn emit(&mut self, amount: u64) {
        self.total_emitted = self.total_emitted.saturating_add(amount);
    }

    /// Whether this ChainState is backed by persistent storage.
    pub fn is_persistent(&self) -> bool {
        self.rocks.is_some()
    }

    /// Feature 121/122/134/135: Attempt a chain reorganization if a competing
    /// chain is longer (or equal length with higher cumulative score).
    /// Returns orphaned transactions that should go back to the mempool.
    /// Refuses to reorg past finality depth or checkpoints.
    pub fn try_reorg(
        &mut self,
        competing_chain: Vec<Block>,
        competing_score: u64,
    ) -> Result<Vec<Transaction>, StateError> {
        if competing_chain.is_empty() {
            return Ok(vec![]);
        }

        let our_height = self.blocks.height();
        let their_height = competing_chain.last().map(|b| b.height()).unwrap_or(0);

        // Must be longer, or equal length with higher cumulative score (feature 134).
        let dominated = their_height > our_height
            || (their_height == our_height && competing_score > self.cumulative_score);
        if !dominated {
            return Ok(vec![]);
        }

        // Find the fork point: lowest height in the competing chain.
        let fork_height = competing_chain.first().map(|b| b.height()).unwrap_or(0);

        // Feature 122: Refuse to reorg past finality depth.
        if our_height >= FINALITY_DEPTH && fork_height < our_height.saturating_sub(FINALITY_DEPTH) {
            warn!(
                "Refusing reorg: fork point {} is below finality depth (current height {})",
                fork_height, our_height
            );
            return Err(StateError::InvalidBlock(
                "reorg blocked by finality depth".into(),
            ));
        }

        // Feature 135: Refuse to reorg past checkpoint blocks.
        // Any checkpoint between fork_height and our_height blocks the reorg.
        let first_checkpoint_after_fork = if fork_height % CHECKPOINT_INTERVAL == 0 {
            fork_height
        } else {
            (fork_height / CHECKPOINT_INTERVAL + 1) * CHECKPOINT_INTERVAL
        };
        if first_checkpoint_after_fork <= our_height {
            warn!(
                "Refusing reorg: checkpoint at height {} is between fork point {} and current height {}",
                first_checkpoint_after_fork, fork_height, our_height
            );
            return Err(StateError::InvalidBlock(
                "reorg blocked by checkpoint".into(),
            ));
        }

        // Collect orphaned transactions from blocks being rolled back.
        let mut orphaned_txs = Vec::new();
        for h in (fork_height..=our_height).rev() {
            if let Some(block) = self.get_block_by_height(h) {
                orphaned_txs.extend(block.transactions.clone());
            }
        }

        // Remove transactions that exist in the new chain (they're not orphaned).
        let new_tx_hashes: std::collections::HashSet<_> = competing_chain
            .iter()
            .flat_map(|b| b.transactions.iter().map(|tx| tx.hash()))
            .collect();
        orphaned_txs.retain(|tx| !new_tx_hashes.contains(&tx.hash()));

        // Roll back: rebuild state up to fork_height - 1.
        // For simplicity, we rebuild the entire chain state from genesis.
        // In production this would use snapshots.
        let saved_rocks = self.rocks.take();
        let _saved_epoch = self.current_epoch;
        let _saved_emitted = self.total_emitted;
        let _saved_burned = self.total_burned;

        // Collect blocks before the fork point.
        let mut pre_fork_blocks = Vec::new();
        for h in 0..fork_height {
            if let Some(block) = self.get_block_by_height(h) {
                pre_fork_blocks.push(block);
            }
        }

        // Reset state.
        self.accounts = AccountStore::new();
        self.blocks = BlockStore::new();
        self.total_emitted = 0;
        self.total_burned = 0;
        self.cumulative_score = 0;

        // Re-apply pre-fork blocks.
        for block in &pre_fork_blocks {
            self.apply_block(block)?;
        }

        // Apply the new competing chain.
        for block in &competing_chain {
            self.apply_block(block)?;
        }

        self.cumulative_score = competing_score;
        self.rocks = saved_rocks;

        info!(
            "Chain reorganization complete: rolled back to height {}, applied {} new blocks (new height {})",
            fork_height.saturating_sub(1),
            competing_chain.len(),
            self.blocks.height(),
        );

        Ok(orphaned_txs)
    }

    /// Persist all meta counters to RocksDB.
    fn flush_meta(&self, rocks: &RocksStore) -> Result<(), StateError> {
        rocks.put_meta_u64(rocks::META_TOTAL_EMITTED, self.total_emitted)
            .map_err(|e| StateError::StorageError(e.to_string()))?;
        rocks.put_meta_u64(rocks::META_TOTAL_BURNED, self.total_burned)
            .map_err(|e| StateError::StorageError(e.to_string()))?;
        rocks.put_meta_u64(rocks::META_CURRENT_EPOCH, self.current_epoch)
            .map_err(|e| StateError::StorageError(e.to_string()))?;
        rocks.put_meta_u64(rocks::META_NERF_RATE_BPS, self.nerf_rate.rate_bps as u64)
            .map_err(|e| StateError::StorageError(e.to_string()))?;
        Ok(())
    }

    /// Flush the full state (all accounts, blocks, meta) to RocksDB.
    fn flush_to_rocks(&self, rocks: &RocksStore) -> Result<(), StateError> {
        // Flush meta.
        self.flush_meta(rocks)?;

        // Flush all accounts. AccountStore doesn't expose iteration,
        // so we use the flush_accounts helper.
        self.flush_accounts(rocks)?;

        Ok(())
    }

    /// Persist all in-memory accounts to RocksDB.
    fn flush_accounts(&self, rocks: &RocksStore) -> Result<(), StateError> {
        for account in self.accounts.iter() {
            rocks.put_account(account)
                .map_err(|e| StateError::StorageError(e.to_string()))?;
        }
        Ok(())
    }
}

impl Default for ChainState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("invalid block height: expected {expected}, got {got}")]
    InvalidHeight { expected: u64, got: u64 },
    #[error("invalid nonce: expected {expected}, got {got}")]
    InvalidNonce { expected: u64, got: u64 },
    #[error("insufficient balance")]
    InsufficientBalance,
    #[error("arithmetic overflow")]
    Overflow,
    #[error("storage error: {0}")]
    StorageError(String),
    #[error("invalid signature: {0}")]
    InvalidSignature(String),
    #[error("invalid block: {0}")]
    InvalidBlock(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use commputer_core::block::{Block, BlockHeader, BlockHash};
    use commputer_core::transaction::{Transaction, TxKind};
    use commputer_core::token::Amount;
    use commputer_core::identity::Address;

    fn addr(n: u8) -> Address {
        let mut a = [0u8; 32];
        a[0] = n;
        Address(a)
    }

    fn genesis_block() -> Block {
        Block {
            header: BlockHeader {
                protocol_version: 1, height: 0,
                parent_hash: BlockHash::GENESIS,
                tx_root: [0u8; 32],
                proof_root: [0u8; 32],
                state_root: [0u8; 32],
                timestamp: 1000,
                producer: addr(0),
                epoch: 0,
                producer_public_key: vec![],
                signature: vec![],
            },
            transactions: vec![],
            proof_summaries: vec![],
        }
    }

    #[test]
    fn initial_state() {
        let state = ChainState::new();
        assert_eq!(state.total_emitted, 0);
        assert_eq!(state.total_burned, 0);
        assert_eq!(state.remaining_supply(), TOTAL_SUPPLY);
        assert_eq!(state.circulating_supply(), 0);
    }

    #[test]
    fn apply_genesis() {
        let mut state = ChainState::new();
        let genesis = genesis_block();
        state.apply_block(&genesis).unwrap();
        assert_eq!(state.blocks.height(), 0);
    }

    #[test]
    fn transfer_updates_balances() {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();

        // Fund sender via emission.
        let sender = state.accounts.get_or_create(addr(1));
        sender.balance = Amount::from_comme(100);

        // Transfer 33 COMME from addr(1) to addr(2).
        let block = Block {
            header: BlockHeader {
                protocol_version: 1, height: 1,
                parent_hash: state.blocks.latest().unwrap().hash(),
                tx_root: [0u8; 32],
                proof_root: [0u8; 32],
                state_root: [0u8; 32],
                timestamp: 2000,
                producer: addr(0),
                epoch: 0,
                producer_public_key: vec![],
                signature: vec![],
            },
            transactions: vec![Transaction {
                from: addr(1),
                nonce: 0,
                kind: TxKind::Transfer {
                    to: addr(2),
                    amount: Amount::from_comme(33),
                },
                fee: 0,
                signature: vec![],
                public_key: vec![],
            }],
            proof_summaries: vec![],
        };
        state.apply_block(&block).unwrap();

        assert_eq!(state.accounts.get(&addr(1)).unwrap().balance, Amount::from_comme(67));
        assert_eq!(state.accounts.get(&addr(2)).unwrap().balance, Amount::from_comme(33));
    }

    #[test]
    fn burn_reduces_supply() {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();

        let sender = state.accounts.get_or_create(addr(1));
        sender.balance = Amount::from_comme(10);
        state.total_emitted = Amount::from_comme(10).raw();

        let block = Block {
            header: BlockHeader {
                protocol_version: 1, height: 1,
                parent_hash: state.blocks.latest().unwrap().hash(),
                tx_root: [0u8; 32],
                proof_root: [0u8; 32],
                state_root: [0u8; 32],
                timestamp: 2000,
                producer: addr(0),
                epoch: 0,
                producer_public_key: vec![],
                signature: vec![],
            },
            transactions: vec![Transaction {
                from: addr(1),
                nonce: 0,
                kind: TxKind::BurstCompute {
                    channel: commputer_core::proof::ResourceChannel::Gpu,
                    burn_amount: Amount::from_comme(5),
                    job_hash: [0u8; 32],
                },
                fee: 0,
                signature: vec![],
                public_key: vec![],
            }],
            proof_summaries: vec![],
        };
        state.apply_block(&block).unwrap();

        assert_eq!(state.total_burned, Amount::from_comme(5).raw());
        assert_eq!(state.circulating_supply(), Amount::from_comme(5).raw());
    }

    #[test]
    fn burst_compute_burns_and_deducts() {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();

        // Fund sender
        let sender = state.accounts.get_or_create(addr(1));
        sender.balance = Amount::from_comme(10);
        state.total_emitted = Amount::from_comme(10).raw();

        // Burst compute: burn 3 COMME
        let block = Block {
            header: BlockHeader {
                protocol_version: 1, height: 1,
                parent_hash: state.blocks.latest().unwrap().hash(),
                tx_root: [0u8; 32],
                proof_root: [0u8; 32],
                state_root: [0u8; 32],
                timestamp: 2000,
                producer: addr(0),
                epoch: 0,
                producer_public_key: vec![],
                signature: vec![],
            },
            transactions: vec![Transaction {
                from: addr(1),
                nonce: 0,
                kind: TxKind::BurstCompute {
                    channel: commputer_core::proof::ResourceChannel::Gpu,
                    burn_amount: Amount::from_comme(3),
                    job_hash: [0u8; 32],
                },
                fee: 0,
                signature: vec![],
                public_key: vec![],
            }],
            proof_summaries: vec![],
        };
        state.apply_block(&block).unwrap();

        // Verify: balance reduced, burn tracked
        assert_eq!(state.accounts.get(&addr(1)).unwrap().balance, Amount::from_comme(7));
        assert_eq!(state.total_burned, Amount::from_comme(3).raw());
        assert_eq!(state.circulating_supply(), Amount::from_comme(7).raw());
    }

    #[test]
    fn burst_compute_insufficient_balance_fails() {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();

        let sender = state.accounts.get_or_create(addr(1));
        sender.balance = Amount::from_comme(2);
        state.total_emitted = Amount::from_comme(2).raw();

        let block = Block {
            header: BlockHeader {
                protocol_version: 1, height: 1,
                parent_hash: state.blocks.latest().unwrap().hash(),
                tx_root: [0u8; 32],
                proof_root: [0u8; 32],
                state_root: [0u8; 32],
                timestamp: 2000,
                producer: addr(0),
                epoch: 0,
                producer_public_key: vec![],
                signature: vec![],
            },
            transactions: vec![Transaction {
                from: addr(1),
                nonce: 0,
                kind: TxKind::BurstCompute {
                    channel: commputer_core::proof::ResourceChannel::Gpu,
                    burn_amount: Amount::from_comme(5), // More than balance
                    job_hash: [0u8; 32],
                },
                fee: 0,
                signature: vec![],
                public_key: vec![],
            }],
            proof_summaries: vec![],
        };
        assert!(state.apply_block(&block).is_err());
    }

    #[test]
    fn chain_state_persists_across_reopen() {
        let dir = tempfile::tempdir().unwrap();

        // Open, apply genesis, fund an account, flush.
        {
            let mut state = ChainState::open(dir.path()).unwrap();
            state.apply_block(&genesis_block()).unwrap();

            // Fund an account.
            let sender = state.accounts.get_or_create(addr(1));
            sender.balance = Amount::from_comme(100);
            state.total_emitted = Amount::from_comme(100).raw();
            state.current_epoch = 5;
            state.flush().unwrap();
        }

        // Reopen and verify state persisted.
        {
            let state = ChainState::open(dir.path()).unwrap();
            assert_eq!(state.blocks.height(), 0);
            assert_eq!(state.blocks.len(), 1);
            let acct = state.accounts.get(&addr(1)).unwrap();
            assert_eq!(acct.balance, Amount::from_comme(100));
            assert_eq!(state.total_emitted, Amount::from_comme(100).raw());
            assert_eq!(state.current_epoch, 5);
        }
    }

    #[test]
    fn chain_state_persists_blocks_on_apply() {
        let dir = tempfile::tempdir().unwrap();

        let genesis_hash;
        // Open, apply genesis + a block with a transfer.
        {
            let mut state = ChainState::open(dir.path()).unwrap();
            state.apply_block(&genesis_block()).unwrap();
            genesis_hash = state.blocks.latest().unwrap().hash();

            // Fund sender.
            let sender = state.accounts.get_or_create(addr(1));
            sender.balance = Amount::from_comme(100);

            // Build and apply a transfer block.
            let block = Block {
                header: BlockHeader {
                    protocol_version: 1, height: 1,
                    parent_hash: genesis_hash,
                    tx_root: [0u8; 32],
                    proof_root: [0u8; 32],
                    state_root: [0u8; 32],
                    timestamp: 2000,
                    producer: addr(0),
                    epoch: 0,
                    producer_public_key: vec![],
                    signature: vec![],
                },
                transactions: vec![Transaction {
                    from: addr(1),
                    nonce: 0,
                    kind: TxKind::Transfer {
                        to: addr(2),
                        amount: Amount::from_comme(33),
                    },
                    fee: 0,
                    signature: vec![],
                    public_key: vec![],
                }],
                proof_summaries: vec![],
            };
            state.apply_block(&block).unwrap();
            // Flush accounts (apply_block persists blocks and meta, but
            // accounts modified outside of apply_block need an explicit flush).
            state.flush().unwrap();
        }

        // Reopen and verify.
        {
            let state = ChainState::open(dir.path()).unwrap();
            assert_eq!(state.blocks.height(), 1);
            assert_eq!(state.blocks.len(), 2);
            assert_eq!(
                state.accounts.get(&addr(1)).unwrap().balance,
                Amount::from_comme(67),
            );
            assert_eq!(
                state.accounts.get(&addr(2)).unwrap().balance,
                Amount::from_comme(33),
            );
        }
    }

    #[test]
    fn in_memory_state_still_works() {
        // Ensure ChainState::new() still works without persistence.
        let mut state = ChainState::new();
        assert!(!state.is_persistent());
        state.apply_block(&genesis_block()).unwrap();
        assert_eq!(state.blocks.height(), 0);
        // flush is a no-op for in-memory.
        state.flush().unwrap();
    }

    #[test]
    fn unsigned_transaction_rejected_by_validated() {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();

        let sender = state.accounts.get_or_create(addr(1));
        sender.balance = Amount::from_comme(100);

        let block = Block {
            header: BlockHeader {
                protocol_version: 1, height: 1,
                parent_hash: state.blocks.latest().unwrap().hash(),
                tx_root: [0u8; 32],
                proof_root: [0u8; 32],
                state_root: [0u8; 32],
                timestamp: 2000,
                producer: addr(0),
                epoch: 0,
                producer_public_key: vec![],
                signature: vec![],
            },
            transactions: vec![Transaction {
                from: addr(1),
                nonce: 0,
                kind: TxKind::Transfer {
                    to: addr(2),
                    amount: Amount::from_comme(10),
                },
                fee: 0,
                signature: vec![], // Empty — should be rejected
                public_key: vec![],
            }],
            proof_summaries: vec![],
        };
        assert!(state.apply_block_validated(&block).is_err());
    }

    #[test]
    fn signed_transaction_accepted_by_validated() {
        use commputer_core::wallet::Wallet;
        use commputer_core::signing::sign_transaction;

        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();

        let wallet = Wallet::generate();
        let sender_addr = *wallet.address();
        let sender = state.accounts.get_or_create(sender_addr);
        sender.balance = Amount::from_comme(100);

        let mut tx = Transaction {
            from: sender_addr,
            nonce: 0,
            kind: TxKind::Transfer {
                to: addr(2),
                amount: Amount::from_comme(10),
            },
            fee: 0,
            signature: vec![],
            public_key: vec![],
        };
        sign_transaction(&mut tx, &wallet);

        let mut block = Block {
            header: BlockHeader {
                protocol_version: 1, height: 1,
                parent_hash: state.blocks.latest().unwrap().hash(),
                tx_root: [0u8; 32],
                proof_root: [0u8; 32],
                state_root: [0u8; 32],
                timestamp: 2000,
                producer: addr(0),
                epoch: 0,
                producer_public_key: vec![],
                signature: vec![],
            },
            transactions: vec![tx],
            proof_summaries: vec![],
        };
        // Compute correct merkle roots before validation.
        block.header.tx_root = block.compute_tx_root();
        block.header.proof_root = block.compute_proof_root();
        assert!(state.apply_block_validated(&block).is_ok());
    }
}
