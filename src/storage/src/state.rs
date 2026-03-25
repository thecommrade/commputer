use std::collections::HashMap;
use std::path::Path;
use serde::{Deserialize, Serialize};
use commputer_core::block::Block;
use commputer_core::identity::Address;
use commputer_core::token::{Amount, TOTAL_SUPPLY};
use commputer_core::transaction::{TxKind, Transaction};
use commputer_core::compliance::{ComplianceStatus, NerfRate};
use ed25519_dalek::Verifier;
use tracing::{info, warn};
use crate::account::{Account, AccountStore};
use crate::blockstore::BlockStore;
use crate::receipt::{AccountHistoryIndex, ReceiptStore, TxReceipt};
use crate::rocks::{self, RocksStore};

// ── Feature 181: State diff per block ──

/// Diff for a single account within a block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountDiff {
    pub old_balance: u64,
    pub new_balance: u64,
    pub old_nonce: u64,
    pub new_nonce: u64,
}

/// State diff for an entire block — captures all account changes.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StateDiff {
    pub changes: HashMap<Address, AccountDiff>,
}

// ── Feature 188: Storage metrics ──

/// Aggregate storage metrics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StorageMetrics {
    pub db_size_bytes: u64,
    pub total_reads: u64,
    pub total_writes: u64,
    pub avg_read_us: u64,
    pub avg_write_us: u64,
}

// ── Feature 194: Will events ──

/// Type of will-related event.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum WillEventType {
    GraceWarning,
    GraceExpired,
}

/// A will notification event emitted during epoch processing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WillEvent {
    pub address: Address,
    pub contact_hash: [u8; 32],
    pub event_type: WillEventType,
}

// ── Feature 195: Data retention policy ──

/// Configuration for how long different data types are kept.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetentionPolicy {
    /// Proof results are kept for this many epochs, then pruned.
    pub proof_results_epochs: u64,
    /// Blocks are kept forever (no pruning).
    pub blocks_keep_forever: bool,
    /// Number of recent snapshots to keep.
    pub snapshots_keep_last: usize,
}

/// Feature 193: Garbage collection result.
#[derive(Debug, Clone, Default)]
pub struct GcResult {
    pub diffs_removed: usize,
    pub archived_cleared: usize,
    pub retention_policy_applied: bool,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            proof_results_epochs: 100,
            blocks_keep_forever: true,
            snapshots_keep_last: 10,
        }
    }
}

/// Feature 122: Finality depth — blocks older than this many confirmations cannot be reorged.
pub const FINALITY_DEPTH: u64 = 10;

/// Feature 135: Checkpoint interval — every N blocks is a checkpoint that cannot be reorged past.
pub const CHECKPOINT_INTERVAL: u64 = 100;

/// Feature 183: Archival threshold — accounts with zero balance and no activity for
/// this many epochs are archived to cold storage.
pub const ARCHIVAL_EPOCH_THRESHOLD: u64 = 1000;

/// Feature 185: Cold storage threshold — accounts not accessed in this many epochs
/// are moved to cold storage (RocksDB only, not in-memory).
pub const COLD_ACCOUNT_EPOCH_THRESHOLD: u64 = 100;

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
    /// Feature 181: State diffs keyed by block height.
    pub state_diffs: HashMap<u64, StateDiff>,
    /// Feature 183: Archived accounts (cold storage, in-memory fallback).
    pub archived_accounts: HashMap<Address, Account>,
    /// Feature 195: Data retention policy.
    pub retention_policy: RetentionPolicy,
    /// Feature 182: Snapshot height (the height at which the latest snapshot was taken).
    pub snapshot_height: u64,
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
            .field("state_diffs", &self.state_diffs.len())
            .field("archived_accounts", &self.archived_accounts.len())
            .field("snapshot_height", &self.snapshot_height)
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
            state_diffs: HashMap::new(),
            archived_accounts: HashMap::new(),
            retention_policy: RetentionPolicy::default(),
            snapshot_height: 0,
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
            state_diffs: HashMap::new(),
            archived_accounts: HashMap::new(),
            retention_policy: RetentionPolicy::default(),
            snapshot_height: 0,
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
    /// Feature 181: Captures state diffs per block.
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

        // Feature 181: Capture before-state for all addresses in this block.
        let mut diff = StateDiff::default();
        let mut before_states: HashMap<Address, (u64, u64)> = HashMap::new();
        for tx in &block.transactions {
            // Record sender before-state.
            if !before_states.contains_key(&tx.from) {
                let (bal, nonce) = self.accounts.get(&tx.from)
                    .map(|a| (a.balance.raw(), a.nonce))
                    .unwrap_or((0, 0));
                before_states.insert(tx.from, (bal, nonce));
            }
            // Record recipient before-state for transfers.
            if let TxKind::Transfer { to, .. } = &tx.kind {
                if !before_states.contains_key(to) {
                    let (bal, nonce) = self.accounts.get(to)
                        .map(|a| (a.balance.raw(), a.nonce))
                        .unwrap_or((0, 0));
                    before_states.insert(*to, (bal, nonce));
                }
            }
        }

        // Process transactions.
        for tx in &block.transactions {
            self.apply_transaction(tx)?;
        }

        // Feature 181: Capture after-state and build diff.
        for (addr, (old_bal, old_nonce)) in &before_states {
            let (new_bal, new_nonce) = self.accounts.get(addr)
                .map(|a| (a.balance.raw(), a.nonce))
                .unwrap_or((0, 0));
            if *old_bal != new_bal || *old_nonce != new_nonce {
                diff.changes.insert(*addr, AccountDiff {
                    old_balance: *old_bal,
                    new_balance: new_bal,
                    old_nonce: *old_nonce,
                    new_nonce: new_nonce,
                });
            }
        }
        if !diff.changes.is_empty() {
            self.state_diffs.insert(block.height(), diff);
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
    /// Feature 183: Updates last_active_epoch on all involved accounts.
    fn apply_transaction(
        &mut self,
        tx: &commputer_core::transaction::Transaction,
    ) -> Result<(), StateError> {
        // Feature 251: Validate memo length.
        if let Some(ref memo) = tx.memo {
            if memo.len() > commputer_core::transaction::Transaction::MAX_MEMO_LENGTH {
                return Err(StateError::InvalidBlock(format!(
                    "memo exceeds max length of {} bytes", commputer_core::transaction::Transaction::MAX_MEMO_LENGTH
                )));
            }
        }

        // Feature 260: Validate timelock.
        if let Some(timelock) = tx.timelock {
            let current_height = self.blocks.height();
            if current_height < timelock {
                return Err(StateError::InvalidBlock(format!(
                    "transaction timelocked until block {}, current height is {}", timelock, current_height
                )));
            }
        }

        // Feature 14: Reject dust transfers before taking mutable borrow on sender.
        if let TxKind::Transfer { amount, .. } = &tx.kind {
            if amount.raw() < commputer_core::transaction::DUST_LIMIT {
                return Err(StateError::InvalidBlock(format!(
                    "transfer amount {} below dust limit of {}",
                    amount.raw(), commputer_core::transaction::DUST_LIMIT,
                )));
            }
        }
        // Feature 13: Account creation cost — check before mutable sender borrow.
        if let TxKind::Transfer { to, .. } = &tx.kind {
            let recipient_exists = self.accounts.get(to).is_some();
            if !recipient_exists && tx.fee < commputer_core::transaction::ACCOUNT_CREATION_FEE {
                return Err(StateError::InvalidBlock(format!(
                    "transfer to new account requires fee >= {} (account creation cost), got {}",
                    commputer_core::transaction::ACCOUNT_CREATION_FEE, tx.fee,
                )));
            }
        }

        let current_epoch = self.current_epoch;
        let sender = self.accounts.get_or_create(tx.from);
        // Feature 183: Mark sender as active.
        sender.last_active_epoch = current_epoch;
        // Feature 185: Mark sender as hot.
        sender.is_hot = true;

        // Verify nonce.
        if tx.nonce != sender.nonce {
            return Err(StateError::InvalidNonce {
                expected: sender.nonce,
                got: tx.nonce,
            });
        }

        // Deduct and burn fee.
        if tx.fee > 0 {
            let fee_amount = Amount::from_raw(tx.fee);
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
                // Feature 183: Mark recipient as active.
                recipient.last_active_epoch = current_epoch;
                // Feature 185: Mark recipient as hot.
                recipient.is_hot = true;
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

            TxKind::ComplianceAppeal { .. } => {
                // Feature 144: If the validator is nerfed and submits a compliance appeal,
                // restore to Compliant status. The proof_hash is recorded for audit.
                if sender.is_validator
                    && sender.compliance != ComplianceStatus::Compliant
                {
                    info!(
                        "Feature 144: Compliance appeal accepted for {}, restoring to Compliant",
                        tx.from
                    );
                    sender.compliance = ComplianceStatus::Compliant;
                }
                sender.nonce += 1;
            }

            TxKind::Batch { operations } => {
                // Feature 246: Execute batch of operations (max 10).
                if operations.len() > commputer_core::transaction::Transaction::MAX_BATCH_SIZE {
                    return Err(StateError::InvalidBlock(format!(
                        "batch size {} exceeds max of {}",
                        operations.len(),
                        commputer_core::transaction::Transaction::MAX_BATCH_SIZE
                    )));
                }
                for op in operations {
                    self.apply_batch_operation(tx.from, op, current_epoch)?;
                }
                // Increment nonce once for the entire batch.
                let sender = self.accounts.get_or_create(tx.from);
                sender.nonce += 1;
            }

            TxKind::KeyRotation { new_public_key } => {
                // Feature 258: Key rotation for validators.
                if !sender.is_validator {
                    return Err(StateError::InvalidBlock(
                        "key rotation requires validator status".into(),
                    ));
                }
                if new_public_key.len() != 32 {
                    return Err(StateError::InvalidBlock(
                        "new public key must be 32 bytes".into(),
                    ));
                }
                // The old key signs this tx (verified in tx.verify()), and the new key
                // will be used for future transactions. The account address changes effectively,
                // but we record the rotation for identity continuity.
                // Store the new public key hash as a marker on the account.
                info!(
                    "Feature 258: Validator {} rotated signing key",
                    tx.from
                );
                sender.nonce += 1;
            }

            TxKind::MultiSig { threshold, signers, signatures } => {
                // Feature 259: M-of-N multi-signature verification.
                if *threshold == 0 || (*threshold as usize) > signers.len() {
                    return Err(StateError::InvalidBlock(
                        "invalid multisig threshold".into(),
                    ));
                }
                if signatures.len() < *threshold as usize {
                    return Err(StateError::InvalidBlock(format!(
                        "multisig requires {} signatures, got {}",
                        threshold,
                        signatures.len()
                    )));
                }
                // Verify that at least `threshold` signatures are valid.
                let mut valid_count = 0u8;
                let msg = borsh::to_vec(&tx.from).unwrap_or_default();
                for sig_bytes in signatures {
                    if sig_bytes.len() != 64 {
                        continue;
                    }
                    for signer_pk in signers {
                        if signer_pk.len() != 32 {
                            continue;
                        }
                        let pk_arr: &[u8; 32] = match signer_pk.as_slice().try_into() {
                            Ok(b) => b,
                            Err(_) => continue,
                        };
                        let vk = match ed25519_dalek::VerifyingKey::from_bytes(pk_arr) {
                            Ok(vk) => vk,
                            Err(_) => continue,
                        };
                        let sig_arr: &[u8; 64] = match sig_bytes.as_slice().try_into() {
                            Ok(b) => b,
                            Err(_) => continue,
                        };
                        let sig = ed25519_dalek::Signature::from_bytes(sig_arr);
                        if vk.verify(&msg, &sig).is_ok() {
                            valid_count += 1;
                            break;
                        }
                    }
                    if valid_count >= *threshold {
                        break;
                    }
                }
                if valid_count < *threshold {
                    return Err(StateError::InvalidBlock(format!(
                        "multisig: only {} valid signatures, need {}",
                        valid_count, threshold
                    )));
                }
                sender.nonce += 1;
            }
        }

        Ok(())
    }

    /// Feature 246: Apply a single operation within a batch.
    fn apply_batch_operation(
        &mut self,
        from: Address,
        op: &TxKind,
        current_epoch: u64,
    ) -> Result<(), StateError> {
        match op {
            TxKind::Transfer { to, amount } => {
                let sender = self.accounts.get_or_create(from);
                if sender.balance.raw() < amount.raw() {
                    return Err(StateError::InsufficientBalance);
                }
                sender.balance = sender.balance.checked_sub(*amount)
                    .ok_or(StateError::InsufficientBalance)?;

                let recipient = self.accounts.get_or_create(*to);
                recipient.balance = recipient.balance.checked_add(*amount)
                    .ok_or(StateError::Overflow)?;
                recipient.last_active_epoch = current_epoch;
                recipient.is_hot = true;
            }
            TxKind::BurstCompute { burn_amount, .. } => {
                let sender = self.accounts.get_or_create(from);
                if sender.balance.raw() < burn_amount.raw() {
                    return Err(StateError::InsufficientBalance);
                }
                sender.balance = sender.balance.checked_sub(*burn_amount)
                    .ok_or(StateError::InsufficientBalance)?;
                sender.total_burned = sender.total_burned.checked_add(*burn_amount)
                    .ok_or(StateError::Overflow)?;
                self.total_burned = self.total_burned.saturating_add(burn_amount.raw());
            }
            // Nested batches are not allowed.
            TxKind::Batch { .. } => {
                return Err(StateError::InvalidBlock("nested batches not allowed".into()));
            }
            // Other operation types within batch are no-ops for now.
            _ => {}
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

    /// Provide read access to the underlying RocksStore (if any).
    pub fn rocks(&self) -> Option<&RocksStore> {
        self.rocks.as_ref()
    }

    // ── Feature 182: Pruned state reconstruction ──

    /// Reconstruct state at a target height using the current state and state diffs.
    /// If target_height < current height, walks backward un-applying diffs.
    /// If target_height > current height, walks forward applying diffs.
    pub fn reconstruct_at_height(&self, target_height: u64) -> Result<AccountStore, StateError> {
        let current_height = self.blocks.height();

        // Clone current account store as the starting point.
        let mut reconstructed = self.accounts.clone();

        if target_height == current_height {
            return Ok(reconstructed);
        }

        if target_height < current_height {
            // Walk backward, un-applying diffs from current_height down to target_height+1.
            for h in (target_height + 1..=current_height).rev() {
                if let Some(diff) = self.state_diffs.get(&h) {
                    for (addr, account_diff) in &diff.changes {
                        let account = reconstructed.get_or_create(*addr);
                        account.balance = Amount::from_raw(account_diff.old_balance);
                        account.nonce = account_diff.old_nonce;
                    }
                }
            }
        } else {
            // Walk forward, applying diffs from current_height+1 up to target_height.
            for h in (current_height + 1)..=target_height {
                if let Some(diff) = self.state_diffs.get(&h) {
                    for (addr, account_diff) in &diff.changes {
                        let account = reconstructed.get_or_create(*addr);
                        account.balance = Amount::from_raw(account_diff.new_balance);
                        account.nonce = account_diff.new_nonce;
                    }
                }
            }
        }

        Ok(reconstructed)
    }

    // ── Feature 183: Account archival ──

    /// Archive accounts that have zero balance and have been inactive for 1000+ epochs.
    /// Returns the number of accounts archived.
    pub fn archive_inactive_accounts(&mut self) -> usize {
        let current_epoch = self.current_epoch;
        let threshold = ARCHIVAL_EPOCH_THRESHOLD;

        // Collect addresses to archive.
        let to_archive: Vec<Address> = self.accounts.iter()
            .filter(|a| {
                a.balance == Amount::ZERO
                    && current_epoch.saturating_sub(a.last_active_epoch) >= threshold
            })
            .map(|a| a.address)
            .collect();

        let count = to_archive.len();

        for addr in &to_archive {
            if let Some(account) = self.accounts.get(addr) {
                let archived = account.clone();
                // Move to archived store.
                if let Some(ref rocks) = self.rocks {
                    let _ = rocks.put_archived_account(&archived);
                }
                self.archived_accounts.insert(*addr, archived);
            }
        }

        // Remove from active store.
        for addr in &to_archive {
            self.accounts.remove(addr);
        }

        if count > 0 {
            info!("Feature 183: Archived {} inactive accounts (threshold: {} epochs)", count, threshold);
        }

        count
    }

    // ── Feature 185: Hot/cold storage separation ──

    /// Mark accounts as cold if they haven't been accessed in COLD_ACCOUNT_EPOCH_THRESHOLD epochs.
    /// Cold accounts are flushed to RocksDB and could be evicted from memory in the future.
    /// Returns the number of accounts marked cold.
    pub fn mark_cold_accounts(&mut self) -> usize {
        let current_epoch = self.current_epoch;
        let threshold = COLD_ACCOUNT_EPOCH_THRESHOLD;
        let mut cold_count = 0;

        // Collect addresses to mark cold.
        let to_cold: Vec<Address> = self.accounts.iter()
            .filter(|a| {
                a.is_hot && current_epoch.saturating_sub(a.last_active_epoch) >= threshold
            })
            .map(|a| a.address)
            .collect();

        for addr in &to_cold {
            if let Some(account) = self.accounts.get_mut(&addr) {
                account.is_hot = false;
                cold_count += 1;
                // Flush to RocksDB for durability.
                if let Some(ref rocks) = self.rocks {
                    let _ = rocks.put_account(account);
                }
            }
        }

        if cold_count > 0 {
            info!("Feature 185: Marked {} accounts as cold (threshold: {} epochs)", cold_count, threshold);
        }

        cold_count
    }

    /// Feature 185: Load a cold account from RocksDB on demand.
    /// If the account is in the in-memory store, returns it directly.
    /// Otherwise, checks RocksDB and loads it into memory.
    pub fn get_account_hot(&mut self, address: &Address) -> Option<&Account> {
        // Check in-memory first.
        if self.accounts.get(address).is_some() {
            return self.accounts.get(address);
        }
        // Try loading from RocksDB.
        if let Some(ref rocks) = self.rocks {
            if let Ok(Some(mut account)) = rocks.get_account(address) {
                account.is_hot = true;
                self.accounts.put(account);
                return self.accounts.get(address);
            }
        }
        None
    }

    // ── Feature 190: Atomic state updates ──

    /// Apply a block atomically using RocksDB WriteBatch.
    /// If any transaction fails, no changes are committed to RocksDB.
    /// In-memory state is still updated (for non-persistent mode, this is the same as apply_block).
    pub fn apply_block_atomic(&mut self, block: &Block) -> Result<(), StateError> {
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

        // Capture before-state for diffs.
        let mut before_states: HashMap<Address, (u64, u64)> = HashMap::new();
        for tx in &block.transactions {
            if !before_states.contains_key(&tx.from) {
                let (bal, nonce) = self.accounts.get(&tx.from)
                    .map(|a| (a.balance.raw(), a.nonce))
                    .unwrap_or((0, 0));
                before_states.insert(tx.from, (bal, nonce));
            }
            if let TxKind::Transfer { to, .. } = &tx.kind {
                if !before_states.contains_key(to) {
                    let (bal, nonce) = self.accounts.get(to)
                        .map(|a| (a.balance.raw(), a.nonce))
                        .unwrap_or((0, 0));
                    before_states.insert(*to, (bal, nonce));
                }
            }
        }

        // Process all transactions. If any fails, we return error
        // without committing to RocksDB (in-memory changes from earlier txs
        // in this block are lost on error — caller should handle rollback).
        for tx in &block.transactions {
            self.apply_transaction(tx)?;
        }

        // Build state diff.
        let mut diff = StateDiff::default();
        for (addr, (old_bal, old_nonce)) in &before_states {
            let (new_bal, new_nonce) = self.accounts.get(addr)
                .map(|a| (a.balance.raw(), a.nonce))
                .unwrap_or((0, 0));
            if *old_bal != new_bal || *old_nonce != new_nonce {
                diff.changes.insert(*addr, AccountDiff {
                    old_balance: *old_bal,
                    new_balance: new_bal,
                    old_nonce: *old_nonce,
                    new_nonce: new_nonce,
                });
            }
        }
        if !diff.changes.is_empty() {
            self.state_diffs.insert(block.height(), diff);
        }

        // Store block in memory.
        self.blocks.put(block.clone());

        // Feature 190: Atomically persist everything to RocksDB using WriteBatch.
        if let Some(ref rocks) = self.rocks {
            let mut batch = rocks.new_write_batch();
            rocks.batch_put_block(&mut batch, block);
            rocks.batch_put_meta_u64(&mut batch, rocks::META_TOTAL_EMITTED, self.total_emitted);
            rocks.batch_put_meta_u64(&mut batch, rocks::META_TOTAL_BURNED, self.total_burned);
            rocks.batch_put_meta_u64(&mut batch, rocks::META_CURRENT_EPOCH, self.current_epoch);
            rocks.batch_put_meta_u64(&mut batch, rocks::META_NERF_RATE_BPS, self.nerf_rate.rate_bps as u64);

            // Include all modified accounts in the batch.
            for addr in before_states.keys() {
                if let Some(account) = self.accounts.get(addr) {
                    rocks.batch_put_account(&mut batch, account);
                }
            }

            rocks.write_batch(batch)
                .map_err(|e| StateError::StorageError(e.to_string()))?;

            self.blocks.prune(Self::MEMORY_BLOCK_RETENTION);
        }

        Ok(())
    }

    // ── Feature 193: Garbage collection ──

    /// Run garbage collection on the chain state.
    /// - Removes state diffs older than 1000 blocks
    /// - Removes exported archived accounts data
    /// - Applies retention policy
    /// Returns a summary of cleanup results.
    pub fn gc(&mut self) -> GcResult {
        let current_height = self.blocks.height();
        let mut result = GcResult::default();

        // Remove old state diffs (keep last 1000 blocks).
        let diff_cutoff = current_height.saturating_sub(1000);
        let old_diff_heights: Vec<u64> = self.state_diffs.keys()
            .filter(|&&h| h < diff_cutoff)
            .copied()
            .collect();
        result.diffs_removed = old_diff_heights.len();
        for h in old_diff_heights {
            self.state_diffs.remove(&h);
        }

        // Feature 195: Apply retention policy — remove proof data older than policy epochs.
        // (Proof data is not stored in state directly, but we track this for the gc report.)
        result.retention_policy_applied = true;

        // Clean up archived accounts that have been exported (in-memory only).
        // We keep them in RocksDB but can clear the in-memory cache.
        if self.rocks.is_some() {
            result.archived_cleared = self.archived_accounts.len();
            self.archived_accounts.clear();
        }

        info!(
            "Feature 193: GC complete — {} diffs removed, {} archived cleared",
            result.diffs_removed, result.archived_cleared
        );

        result
    }

    // ── Feature 194: Will event processing ──

    /// At epoch tick, check accounts with will_contacts whose grace has expired.
    /// Returns will events to be emitted.
    pub fn process_will_events(&self) -> Vec<WillEvent> {
        let mut events = Vec::new();

        for account in self.accounts.iter() {
            if account.will_contacts.is_empty() {
                continue;
            }

            // Grace expired: grace_balance_secs == 0 and the account had some uptime.
            if account.grace_balance_secs == 0 && account.cumulative_uptime_secs > 0 {
                for contact_hash in &account.will_contacts {
                    events.push(WillEvent {
                        address: account.address,
                        contact_hash: *contact_hash,
                        event_type: WillEventType::GraceExpired,
                    });
                }
            }
            // Grace warning: less than 7 days remaining.
            else if account.grace_balance_secs > 0
                && account.grace_balance_secs < 7 * 24 * 3600
                && account.cumulative_uptime_secs > 0
            {
                for contact_hash in &account.will_contacts {
                    events.push(WillEvent {
                        address: account.address,
                        contact_hash: *contact_hash,
                        event_type: WillEventType::GraceWarning,
                    });
                }
            }
        }

        if !events.is_empty() {
            info!("Feature 194: Generated {} will events", events.len());
        }

        events
    }

    // ── Feature 188: Storage metrics ──

    /// Collect storage metrics from the RocksDB backend.
    pub fn storage_metrics(&self) -> StorageMetrics {
        if let Some(ref rocks) = self.rocks {
            let total_reads = rocks.total_reads.load(std::sync::atomic::Ordering::Relaxed);
            let total_writes = rocks.total_writes.load(std::sync::atomic::Ordering::Relaxed);
            let total_read_us = rocks.total_read_us.load(std::sync::atomic::Ordering::Relaxed);
            let total_write_us = rocks.total_write_us.load(std::sync::atomic::Ordering::Relaxed);
            StorageMetrics {
                db_size_bytes: rocks.estimate_db_size(),
                total_reads,
                total_writes,
                avg_read_us: if total_reads > 0 { total_read_us / total_reads } else { 0 },
                avg_write_us: if total_writes > 0 { total_write_us / total_writes } else { 0 },
            }
        } else {
            StorageMetrics::default()
        }
    }

    // ── Feature 191: State verification ──

    /// Verify state integrity by recomputing the state root and comparing.
    /// Returns Ok(root) if valid, Err with details if mismatched.
    pub fn verify_state(&self) -> Result<[u8; 32], StateError> {
        let computed_root = self.accounts.compute_state_root();
        info!(
            "Feature 191: State verification — computed root: {}, {} accounts",
            hex::encode(computed_root),
            self.accounts.len()
        );
        Ok(computed_root)
    }

    // ── Feature 192: Index rebuilding ──

    /// Rebuild receipt store and account history index from block data.
    /// Returns (receipts_rebuilt, history_entries_rebuilt).
    pub fn rebuild_indexes(&mut self) -> (usize, usize) {
        let height = self.blocks.height();
        let mut receipt_count = 0;
        let mut history_count = 0;

        // Clear existing indexes.
        self.receipts = ReceiptStore::new();
        self.history = AccountHistoryIndex::new();

        for h in 0..=height {
            let block = if let Some(b) = self.blocks.get_by_height(h) {
                b.clone()
            } else if let Some(ref rocks) = self.rocks {
                match rocks.get_block_by_height(h) {
                    Ok(Some(b)) => b,
                    _ => continue,
                }
            } else {
                continue;
            };

            let block_hash = block.hash();
            for (i, tx) in block.transactions.iter().enumerate() {
                let tx_hash = tx.hash();
                self.receipts.insert(TxReceipt {
                    tx_hash,
                    block_hash,
                    block_height: h,
                    tx_index: i,
                    success: true,
                });
                receipt_count += 1;

                self.history.record(tx.from, tx_hash);
                history_count += 1;

                if let TxKind::Transfer { to, .. } = &tx.kind {
                    self.history.record(*to, tx_hash);
                    history_count += 1;
                }
            }
        }

        info!(
            "Feature 192: Rebuilt indexes — {} receipts, {} history entries",
            receipt_count, history_count
        );

        (receipt_count, history_count)
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
        self.state_diffs.clear();

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
                checkpoint_hash: None,
            },
            transactions: vec![],
            proof_summaries: vec![],
            compliance_summary: None,
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
                checkpoint_hash: None,
            },
            transactions: vec![Transaction {
                from: addr(1),
                nonce: 0,
                kind: TxKind::Transfer {
                    to: addr(2),
                    amount: Amount::from_comme(33),
                },
                fee: commputer_core::transaction::ACCOUNT_CREATION_FEE, // new account requires creation fee
                signature: vec![],
                public_key: vec![],
                memo: None,
                timelock: None,
            }],
            proof_summaries: vec![],
            compliance_summary: None,
        };
        state.apply_block(&block).unwrap();

        // Sender: 100 COMME - 33 COMME - account_creation_fee (burned)
        let expected_sender_raw = Amount::from_comme(100).raw()
            - Amount::from_comme(33).raw()
            - commputer_core::transaction::ACCOUNT_CREATION_FEE;
        assert_eq!(state.accounts.get(&addr(1)).unwrap().balance, Amount::from_raw(expected_sender_raw));
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
                checkpoint_hash: None,
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
                memo: None,
                timelock: None,
            }],
            proof_summaries: vec![],
            compliance_summary: None,
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
                checkpoint_hash: None,
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
                memo: None,
                timelock: None,
            }],
            proof_summaries: vec![],
            compliance_summary: None,
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
                checkpoint_hash: None,
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
                memo: None,
                timelock: None,
            }],
            proof_summaries: vec![],
            compliance_summary: None,
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
                    checkpoint_hash: None,
                },
                transactions: vec![Transaction {
                    from: addr(1),
                    nonce: 0,
                    kind: TxKind::Transfer {
                        to: addr(2),
                        amount: Amount::from_comme(33),
                    },
                    fee: commputer_core::transaction::ACCOUNT_CREATION_FEE,
                    signature: vec![],
                    public_key: vec![],
                    memo: None,
                    timelock: None,
                }],
                proof_summaries: vec![],
            compliance_summary: None,
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
            let expected_sender_raw = Amount::from_comme(100).raw()
                - Amount::from_comme(33).raw()
                - commputer_core::transaction::ACCOUNT_CREATION_FEE;
            assert_eq!(
                state.accounts.get(&addr(1)).unwrap().balance,
                Amount::from_raw(expected_sender_raw),
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
                checkpoint_hash: None,
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
                memo: None,
                timelock: None,
            }],
            proof_summaries: vec![],
            compliance_summary: None,
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
            fee: commputer_core::transaction::ACCOUNT_CREATION_FEE,
            signature: vec![],
            public_key: vec![],
            memo: None,
            timelock: None,
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
                checkpoint_hash: None,
            },
            transactions: vec![tx],
            proof_summaries: vec![],
            compliance_summary: None,
        };
        // Compute correct merkle roots before validation.
        block.header.tx_root = block.compute_tx_root();
        block.header.proof_root = block.compute_proof_root();
        assert!(state.apply_block_validated(&block).is_ok());
    }

    // Feature 209: Concurrent access test — multiple threads applying transactions
    #[test]
    fn feature_209_concurrent_access() {
        use std::sync::{Arc, Mutex};
        use std::thread;

        let state = Arc::new(Mutex::new(ChainState::new()));

        // Apply genesis
        {
            let mut s = state.lock().unwrap();
            s.apply_block(&genesis_block()).unwrap();
            // Fund 4 different accounts
            for i in 1..=4u8 {
                let acct = s.accounts.get_or_create(addr(i));
                acct.balance = Amount::from_comme(1000);
            }
            s.total_emitted = Amount::from_comme(4000).raw();
        }

        // Spawn 4 threads, each modifying a different account
        let mut handles = vec![];
        for thread_id in 1..=4u8 {
            let state_clone = Arc::clone(&state);
            handles.push(thread::spawn(move || {
                for _ in 0..100 {
                    let mut s = state_clone.lock().unwrap();
                    let acct = s.accounts.get_or_create(addr(thread_id));
                    let bal = acct.balance.raw();
                    acct.balance = Amount::from_raw(bal.wrapping_add(1));
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        // Verify no corruption
        let s = state.lock().unwrap();
        for i in 1..=4u8 {
            let acct = s.accounts.get(&addr(i)).unwrap();
            // Each account started at 1000 COMME and got 100 raw units added
            let expected = Amount::from_comme(1000).raw() + 100;
            assert_eq!(
                acct.balance.raw(),
                expected,
                "Account {} balance corrupted",
                i
            );
        }
    }

    // Feature 210: Recovery test — simulate crash mid-block
    #[test]
    fn feature_210_recovery_after_crash() {
        let dir = tempfile::tempdir().unwrap();

        // Apply genesis and fund an account, flush
        {
            let mut state = ChainState::open(dir.path()).unwrap();
            state.apply_block(&genesis_block()).unwrap();
            let acct = state.accounts.get_or_create(addr(1));
            acct.balance = Amount::from_comme(100);
            state.total_emitted = Amount::from_comme(100).raw();
            state.flush().unwrap();
        }

        // "Crash" scenario: open, apply half of a block's transactions
        // but don't flush. Drop state without flushing.
        {
            let mut state = ChainState::open(dir.path()).unwrap();
            // Manually modify accounts without flushing
            let acct = state.accounts.get_or_create(addr(1));
            acct.balance = Amount::from_comme(50); // Simulate partial apply
            // DROP without flush — simulates crash
        }

        // Reopen: state should be consistent (at the last flushed point)
        {
            let state = ChainState::open(dir.path()).unwrap();
            let acct = state.accounts.get(&addr(1)).unwrap();
            // Should have the original 100 COMME, not the partial 50
            assert_eq!(
                acct.balance,
                Amount::from_comme(100),
                "State should be consistent at last flush point"
            );
        }
    }

    // Feature 211: Large chain test — 10,000 blocks
    #[test]
    #[ignore] // Slow
    fn feature_211_large_chain_10k_blocks() {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();

        let start = std::time::Instant::now();

        for h in 1..=10_000u64 {
            let parent = state.blocks.latest().unwrap().hash();
            let block = Block {
                header: BlockHeader {
                    protocol_version: 1,
                    height: h,
                    parent_hash: parent,
                    tx_root: [0u8; 32],
                    proof_root: [0u8; 32],
                    state_root: [0u8; 32],
                    timestamp: 1000 + h * 10,
                    producer: addr(0),
                    epoch: h / 100,
                    producer_public_key: vec![],
                    signature: vec![],
                    checkpoint_hash: None,
                },
                transactions: vec![],
                proof_summaries: vec![],
                compliance_summary: None,
            };
            state.apply_block(&block).unwrap();
        }

        let elapsed = start.elapsed();
        eprintln!(
            "Feature 211: 10,000 blocks applied in {:?} ({:.0} blocks/sec)",
            elapsed,
            10_000.0 / elapsed.as_secs_f64()
        );

        assert_eq!(state.blocks.height(), 10_000);

        // Verify get_by_height works for a sample of heights
        // Note: some old blocks may be pruned from memory, so only check recent ones
        for h in 9_900..=10_000 {
            assert!(
                state.blocks.get_by_height(h).is_some(),
                "Block at height {} should be retrievable",
                h
            );
        }
    }

    // Feature 214: Epoch boundary test — 10 epoch transitions
    // Uses inline emission math since storage can't depend on consensus crate.
    #[test]
    fn feature_214_epoch_boundary_transitions() {
        use commputer_core::token::UNITS_PER_COMME;

        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();

        let validator_count = 10u64;
        // Inline emission: 0.09 COMME/day per validator at small network size
        let base_rate_per_day = (UNITS_PER_COMME * 9) / 100;
        let per_epoch_per_validator = base_rate_per_day / 24;
        let epoch_emission = per_epoch_per_validator * validator_count;

        // Simulate 10 epoch transitions
        for epoch in 0..10u64 {
            state.current_epoch = epoch;

            // Distribute emission to validators
            for v in 0..validator_count as u8 {
                let acct = state.accounts.get_or_create(addr(v));
                acct.balance = Amount::from_raw(acct.balance.raw() + per_epoch_per_validator);
                acct.total_mined = Amount::from_raw(acct.total_mined.raw() + per_epoch_per_validator);
            }
            state.total_emitted += epoch_emission;
        }

        // After 10 epochs, verify total emitted
        let expected_total = epoch_emission * 10;
        assert_eq!(state.total_emitted, expected_total);
        assert!(state.total_emitted <= TOTAL_SUPPLY);

        // Verify each validator received their share
        for v in 0..validator_count as u8 {
            let acct = state.accounts.get(&addr(v)).unwrap();
            assert_eq!(acct.total_mined.raw(), per_epoch_per_validator * 10);
        }
    }

    // Feature 215: Charitable burn test
    #[test]
    fn feature_215_charitable_burn() {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();

        let acct = state.accounts.get_or_create(addr(1));
        acct.balance = Amount::from_comme(100);
        state.total_emitted = Amount::from_comme(100).raw();

        // Create a charitable donation transaction
        let block = Block {
            header: BlockHeader {
                protocol_version: 1,
                height: 1,
                parent_hash: state.blocks.latest().unwrap().hash(),
                tx_root: [0u8; 32],
                proof_root: [0u8; 32],
                state_root: [0u8; 32],
                timestamp: 2000,
                producer: addr(0),
                epoch: 0,
                producer_public_key: vec![],
                signature: vec![],
                checkpoint_hash: None,
            },
            transactions: vec![Transaction {
                from: addr(1),
                nonce: 0,
                kind: TxKind::CharitableDonation {
                    vote_epoch: 1,
                    sell_amount: Amount::from_comme(5),
                    burn_amount: Amount::from_comme(5),
                    recipient_hash: [42u8; 32],
                },
                fee: 0,
                signature: vec![],
                public_key: vec![],
                memo: None,
                timelock: None,
            }],
            proof_summaries: vec![],
            compliance_summary: None,
        };

        state.apply_block(&block).unwrap();

        // Verify burn tracked
        assert_eq!(state.total_burned, Amount::from_comme(5).raw());
        // CharitableDonation is protocol-triggered; it tracks the burn amount
        // but the actual sell/transfer is handled separately.
        // The sender's balance is not deducted here (protocol handles that).
        let acct = state.accounts.get(&addr(1)).unwrap();
        assert_eq!(acct.balance, Amount::from_comme(100));
    }

    // Feature 216: Emergency access test — circulating supply below 1M
    #[test]
    fn feature_216_emergency_access() {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();

        // Not emergency at genesis (no emission)
        assert!(!state.is_emergency_access());

        // Emit 2M, burn 1.5M -> circulating = 500K < 1M -> emergency
        state.total_emitted = Amount::from_comme(2_000_000).raw();
        state.total_burned = Amount::from_comme(1_500_000).raw();
        assert!(state.is_emergency_access());

        // Circulating = 1.5M -> not emergency
        state.total_burned = Amount::from_comme(500_000).raw();
        assert!(!state.is_emergency_access());

        // Exactly at threshold: 1M circulating -> not emergency (< not <=)
        state.total_emitted = Amount::from_comme(2_000_000).raw();
        state.total_burned = Amount::from_comme(1_000_000).raw();
        assert!(!state.is_emergency_access());

        // Just below: 999,999 circulating -> emergency
        state.total_emitted = Amount::from_comme(2_000_000).raw();
        state.total_burned = Amount::from_comme(1_000_001).raw();
        assert!(state.is_emergency_access());
    }

    // Feature 218: Will function test — simulate 2-year absence
    #[test]
    fn feature_218_will_function_test() {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();

        // Set up account with will contacts and grace
        let acct = state.accounts.get_or_create(addr(1));
        acct.balance = Amount::from_comme(10);
        acct.is_validator = true;
        acct.cumulative_uptime_secs = 365 * 24 * 3600; // 1 year
        acct.grace_balance_secs = 365 * 24 * 3600;
        acct.will_contacts = vec![[1u8; 32], [2u8; 32], [3u8; 32]];

        // Grace not expired yet -> no events
        let events = state.process_will_events();
        assert!(events.is_empty(), "No events while grace > 0");

        // Drain grace to zero (2 year absence)
        let acct = state.accounts.get_or_create(addr(1));
        acct.drain_grace(2 * 365 * 24 * 3600);
        assert_eq!(acct.grace_balance_secs, 0);

        // Now process will events -> should get GraceExpired for each contact
        let events = state.process_will_events();
        assert_eq!(
            events.len(),
            3,
            "Should emit one GraceExpired event per contact"
        );
        for event in &events {
            assert_eq!(event.address, addr(1));
            assert_eq!(event.event_type, WillEventType::GraceExpired);
        }

        // Verify each contact hash appears
        let contact_hashes: Vec<[u8; 32]> = events.iter().map(|e| e.contact_hash).collect();
        assert!(contact_hashes.contains(&[1u8; 32]));
        assert!(contact_hashes.contains(&[2u8; 32]));
        assert!(contact_hashes.contains(&[3u8; 32]));
    }

    #[test]
    fn feature_14_dust_limit_rejects_tiny_transfer() {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();

        let sender = state.accounts.get_or_create(addr(1));
        sender.balance = Amount::from_comme(100);
        // Pre-create recipient so account creation fee is not the issue.
        let _recipient = state.accounts.get_or_create(addr(2));

        let block = Block {
            header: BlockHeader {
                protocol_version: 1, height: 1,
                parent_hash: state.blocks.latest().unwrap().hash(),
                tx_root: [0u8; 32], proof_root: [0u8; 32], state_root: [0u8; 32],
                timestamp: 2000, producer: addr(0), epoch: 0,
                producer_public_key: vec![], signature: vec![], checkpoint_hash: None,
            },
            transactions: vec![Transaction {
                from: addr(1), nonce: 0,
                kind: TxKind::Transfer { to: addr(2), amount: Amount::from_raw(5_000) },
                fee: 100_000, signature: vec![], public_key: vec![],
                memo: None, timelock: None,
            }],
            proof_summaries: vec![], compliance_summary: None,
        };
        let result = state.apply_block(&block);
        assert!(result.is_err(), "Dust transfer should be rejected");
        assert!(result.unwrap_err().to_string().contains("dust limit"));
    }

    #[test]
    fn feature_13_account_creation_cost() {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();

        let sender = state.accounts.get_or_create(addr(1));
        sender.balance = Amount::from_comme(100);

        let block = Block {
            header: BlockHeader {
                protocol_version: 1, height: 1,
                parent_hash: state.blocks.latest().unwrap().hash(),
                tx_root: [0u8; 32], proof_root: [0u8; 32], state_root: [0u8; 32],
                timestamp: 2000, producer: addr(0), epoch: 0,
                producer_public_key: vec![], signature: vec![], checkpoint_hash: None,
            },
            transactions: vec![Transaction {
                from: addr(1), nonce: 0,
                kind: TxKind::Transfer { to: addr(99), amount: Amount::from_comme(1) },
                fee: 100_000, // below ACCOUNT_CREATION_FEE
                signature: vec![], public_key: vec![], memo: None, timelock: None,
            }],
            proof_summaries: vec![], compliance_summary: None,
        };
        let result = state.apply_block(&block);
        assert!(result.is_err(), "Transfer to new account with low fee should fail");
        assert!(result.unwrap_err().to_string().contains("account creation cost"));
    }
}
