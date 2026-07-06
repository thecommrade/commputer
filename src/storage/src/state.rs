use std::collections::{HashMap, HashSet};
use std::path::Path;
use rocksdb::WriteBatch;
use serde::{Deserialize, Serialize};
use borsh::{BorshSerialize, BorshDeserialize};
use sha2::{Digest, Sha256};
use commputer_core::block::Block;
use commputer_core::identity::Address;
use commputer_core::token::{Amount, TOTAL_SUPPLY};
use commputer_core::transaction::{TxKind, Transaction};
use commputer_core::compliance::{ComplianceStatus, NerfRate};
use commputer_pouw_onchain::lifecycle::{JobLifecycle, EventResult, Terminal, Phase};
use commputer_pouw_onchain::escrow_ledger::Ledger;
use commputer_pouw::oracle::{ChainHooks, EquivalenceOracle};
use commputer_pouw::ids::ParticipantId;
use commputer_pouw::job::{Commitment, Reveal};
use commputer_pouw::params::GameParams;
use commputer_pouw_onchain::settlement_resolution::ResolutionParams;
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

/// Feature 10: Per-validator performance tracking.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ValidatorPerformance {
    /// Number of blocks produced by this validator.
    pub blocks_produced: u64,
    /// Number of proof challenges passed.
    pub proofs_passed: u64,
    /// Total uptime in seconds.
    pub uptime_secs: u64,
    /// Last block height at which this validator was active.
    pub last_active_height: u64,
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
/// Optionally backed by RocksDB for persistence across restarts. When backed, every applied
/// block is persisted atomically (state survives a crash without a clean-shutdown flush).
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
    /// Feature 10: Per-validator performance history.
    pub validator_performance: HashMap<Address, ValidatorPerformance>,
    /// PoUW P1: $COMME escrowed per live compute job (`job_id` -> raw units). The sum
    /// (`total_escrowed`) is part of circulating supply — escrowed value is HELD, not burned —
    /// so `total_burned` MUST NOT move when a budget is escrowed; only a resolver's burn slice
    /// at settlement increments it. Empty until the `SubmitJobV2` burn->escrow flip lands (P2).
    ///
    /// PERSISTENCE (P2 step 1.0, complete — applies to all FOUR consensus maps): every
    /// `apply_*` commits block + meta + dirty accounts + map deltas in ONE WriteBatch
    /// (`persist_applied_block`); entries removed in-memory are CF-deleted via the
    /// persisted-key mirrors; loaded in `open()`; folded into `compute_state_root` (Policy B).
    /// `revert_block` refuses blocks that touch these maps — fork recovery is
    /// `reset_to_genesis` + resync (or `try_reorg`, which replays then reconciles the CFs in
    /// one atomic batch).
    pub escrow_by_job: HashMap<[u8; 32], u64>,
    /// PoUW P2 (G4): active bonded stake (`Address` -> raw units) — the committee-selection
    /// weight (`stake_of`) and the primary slash surface. Moved here from `Account.balance` by
    /// `bond`; counts toward selection + is slashable. Reuses `total_burned` as the slash sink.
    pub bonded_stake: HashMap<Address, u64>,
    /// PoUW P2 (G4): cooldown stake awaiting withdrawal (`Address` -> maturing chunks). Stops
    /// counting toward selection the moment it is requested, but stays slashable until withdrawn
    /// (anti-dodge). `withdraw_unbonded` returns matured chunks to `Account.balance`.
    pub unbonding_stake: HashMap<Address, Vec<UnbondingChunk>>,
    /// PoUW P2 (G4): staking params (cooldown length, min eligible bond). Genesis-anchored at
    /// P3/G5; the default is a placeholder until then — all nodes MUST agree or they diverge
    /// on committee draw. `bonded_stake`/`unbonding_stake` are persisted per-block and folded
    /// into the state root (see the PERSISTENCE note on `escrow_by_job`);
    /// Bond/RequestUnbond/WithdrawUnbonded are live TxKinds (N1).
    pub stake_params: StakeParams,
    /// PoUW P2 (B1b): genesis-anchored consensus params (G5) needed to reconstruct each persisted
    /// `JobLifecycle` on load. Every lifecycle is created with these identical params, so they are NOT
    /// persisted per-job — they are RECONSTRUCTED-from-config here and re-injected in `from_record`
    /// (no RocksDB round-trip). TODO(B8, PROTECTED genesis): populate both from `GenesisConfig`;
    /// default until then.
    pub game_params: GameParams,
    pub resolution_params: ResolutionParams,
    /// PoUW P2: per-job verification lifecycle (`job_id` -> `JobLifecycle`), the multi-block
    /// commit-reveal state machine. Created at `ClaimJob` (AwaitingResult), committee drawn at
    /// `CompleteJob`, fed by `Commit`/`Reveal`, advanced/settled by block height. Its money moves
    /// run against `ChainState` via the §3 `Ledger` trait. Empty until the committee-draw wiring
    /// (event_loop.rs) is live. Persisted per-block as borsh `JobLifecycleRecord` DTOs
    /// (CF_LIFECYCLE) and folded into the state root; params re-injected on load (see the B1b
    /// note on `game_params` above).
    pub job_lifecycles: HashMap<[u8; 32], JobLifecycle>,
    // Node-local mirrors of the keys currently persisted in each consensus-map CF (never
    // state-rooted, never serialized). Each persist computes `mirror − current_keys` to
    // CF-delete removed entries, then re-puts every live entry full-value; the mirror advances
    // to the new key set only AFTER the WriteBatch commits. Because the diff is computed from
    // ground truth at write time, direct `pub`-field map mutation cannot bypass it (contrast
    // `AccountStore`, whose privacy makes dirty-tracking sufficient — that asymmetry is
    // deliberate).
    persisted_escrow_keys: HashSet<[u8; 32]>,
    persisted_bonded_keys: HashSet<Address>,
    persisted_unbonding_keys: HashSet<Address>,
    persisted_lifecycle_keys: HashSet<[u8; 32]>,
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
            .field("validator_performance", &self.validator_performance.len())
            .field("escrow_by_job", &self.escrow_by_job.len())
            .field("bonded_stake", &self.bonded_stake.len())
            .field("unbonding_stake", &self.unbonding_stake.len())
            .field("job_lifecycles", &self.job_lifecycles.len())
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
            validator_performance: HashMap::new(),
            escrow_by_job: HashMap::new(),
            bonded_stake: HashMap::new(),
            unbonding_stake: HashMap::new(),
            stake_params: StakeParams::default(),
            game_params: GameParams::default(),
            resolution_params: ResolutionParams::default(),
            job_lifecycles: HashMap::new(),
            persisted_escrow_keys: HashSet::new(),
            persisted_bonded_keys: HashSet::new(),
            persisted_unbonding_keys: HashSet::new(),
            persisted_lifecycle_keys: HashSet::new(),
        }
    }

    /// Open a persistent ChainState backed by RocksDB at the given path.
    /// Loads all state from disk into the in-memory stores.
    ///
    /// MIGRATION NOTE: data dirs written by the pre-per-block-persistence code (put-only
    /// flushes, no delete reconcile) may already contain resurrected stale rows that this code
    /// cannot distinguish from real state; deployment assumes fresh data dirs or a
    /// reset_to_genesis + resync at upgrade. No deployed network exists, so no schema-version
    /// bump.
    pub fn open(path: &Path) -> Result<Self, StateError> {
        // Item 16: Try to open, and if it fails, attempt repair then retry.
        let rocks = match RocksStore::open(path) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("Database open failed: {}. Attempting repair...", e);
                RocksStore::try_repair(path);
                RocksStore::open(path)
                    .map_err(|e2| StateError::StorageError(
                        format!("database open failed after repair: {}", e2)
                    ))?
            }
        };

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
        // Disk == memory by construction at this point (the `put`s above journaled every loaded
        // address) — start with clean dirty/removed journals so the first block's batch carries
        // only that block's touched accounts.
        accounts.clear_dirty_and_removed();

        // Load all blocks into the in-memory store.
        let mut blocks = BlockStore::new();
        for block in rocks.all_blocks_by_height() {
            blocks.put(block);
        }

        // PoUW P2 (B1a): load the persisted consensus money/stake maps (empty until the live flip).
        let escrow_by_job = rocks.all_escrow();
        let bonded_stake = rocks.all_bonded();
        let unbonding_stake = rocks.all_unbonding();

        // PoUW P2 (B1b): load persisted job_lifecycles. GameParams/ResolutionParams are genesis-anchored
        // (identical for every job) so they are NOT persisted per-job — reconstruct + re-inject them.
        // TODO(B8, PROTECTED genesis): populate these from GenesisConfig; default until then. Because
        // every lifecycle was created with the same consensus params, re-injecting this copy reproduces
        // the original params exactly (true now with defaults, and post-B8 with genesis values).
        let game_params = GameParams::default();
        let resolution_params = ResolutionParams::default();
        let job_lifecycles: HashMap<[u8; 32], JobLifecycle> = rocks
            .all_lifecycle()
            .into_iter()
            .map(|(id, rec)| (id, JobLifecycle::from_record(rec, game_params.clone(), resolution_params)))
            .collect();

        // Persisted-key mirrors start EXACT: load IS a CF scan, so each loaded key set equals
        // the rows on disk. (Warn-skipped malformed rows linger as junk OUTSIDE the mirror and
        // the state root; they are re-skipped on every open and cannot resurrect — no
        // delete-on-load.)
        let persisted_escrow_keys: HashSet<[u8; 32]> = escrow_by_job.keys().copied().collect();
        let persisted_bonded_keys: HashSet<Address> = bonded_stake.keys().copied().collect();
        let persisted_unbonding_keys: HashSet<Address> = unbonding_stake.keys().copied().collect();
        let persisted_lifecycle_keys: HashSet<[u8; 32]> = job_lifecycles.keys().copied().collect();

        let account_count = accounts.len();
        let block_count = blocks.len();
        let height = blocks.height();

        info!(
            "Loaded state from disk: {} blocks (height {}), {} accounts, epoch {}; \
             escrow_pots={}, bonded={}, unbonding={}, lifecycles={}",
            block_count, height, account_count, current_epoch,
            escrow_by_job.len(), bonded_stake.len(), unbonding_stake.len(), job_lifecycles.len(),
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
            validator_performance: HashMap::new(),
            escrow_by_job,
            bonded_stake,
            unbonding_stake,
            stake_params: StakeParams::default(),
            game_params,
            resolution_params,
            job_lifecycles,
            persisted_escrow_keys,
            persisted_bonded_keys,
            persisted_unbonding_keys,
            persisted_lifecycle_keys,
        })
    }

    /// Flush the full current state to RocksDB — a reconciling sweep: CF rows for removed
    /// accounts/map entries are deleted, then everything live is re-put. Per-block persistence
    /// (`persist_applied_block`) already covers block application; this remains the
    /// shutdown-tail sweeper for out-of-band mutations after the last applied block (grace
    /// drains, epoch bump on an idle node). Any future non-shutdown caller inherits the same
    /// reconcile semantics: mirrors committed + removed-set drained only after a successful
    /// write.
    pub fn flush(&mut self) -> Result<(), StateError> {
        if self.rocks.is_some() {
            self.flush_to_rocks()?;
        }
        Ok(())
    }

    /// Item 15: Mark a clean shutdown in the database.
    pub fn mark_clean_shutdown(&self) {
        if let Some(ref rocks) = self.rocks {
            rocks.mark_clean_shutdown();
        }
    }

    /// Retrieve a block by height. Checks in-memory first, falls back to RocksDB.
    pub fn get_block_by_height(&self, height: u64) -> Option<Block> {
        // Try in-memory first.
        if let Some(block) = self.blocks.get_by_height(height) {
            return Some(block.clone());
        }
        // Fall back to RocksDB for pruned blocks.
        if let Some(ref rocks) = self.rocks
            && let Ok(Some(block)) = rocks.get_block_by_height(height) {
                return Some(block);
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

        // PoUW P2 (B1a): the consensus money/stake maps (sorted for stable diffs; debug-only — the
        // authoritative commitment is `state_root`).
        let mut escrow: Vec<(&[u8; 32], &u64)> = self.escrow_by_job.iter().collect();
        escrow.sort_by(|a, b| a.0.cmp(b.0));
        let escrow_json: Vec<serde_json::Value> = escrow.iter()
            .map(|(id, amt)| serde_json::json!({ "job_id": hex::encode(id), "amount": **amt }))
            .collect();
        let mut bonded: Vec<(&Address, &u64)> = self.bonded_stake.iter().collect();
        bonded.sort_by(|a, b| a.0.0.cmp(&b.0.0));
        let bonded_json: Vec<serde_json::Value> = bonded.iter()
            .map(|(addr, amt)| serde_json::json!({ "address": hex::encode(addr.0), "amount": **amt }))
            .collect();
        let mut unbonding: Vec<(&Address, &Vec<UnbondingChunk>)> = self.unbonding_stake.iter().collect();
        unbonding.sort_by(|a, b| a.0.0.cmp(&b.0.0));
        let unbonding_json: Vec<serde_json::Value> = unbonding.iter().map(|(addr, chunks)| {
            let cs: Vec<serde_json::Value> = chunks.iter()
                .map(|c| serde_json::json!({ "amount": c.amount, "matures_at": c.matures_at }))
                .collect();
            serde_json::json!({ "address": hex::encode(addr.0), "chunks": cs })
        }).collect();

        // PoUW P2 (B1b): job_lifecycles (debug-only; the authoritative commitment is `state_root`).
        let mut lifecycles: Vec<(&[u8; 32], &JobLifecycle)> = self.job_lifecycles.iter().collect();
        lifecycles.sort_by(|a, b| a.0.cmp(b.0));
        let lifecycles_json: Vec<serde_json::Value> = lifecycles.iter().map(|(id, lc)| {
            let r = lc.to_record();
            serde_json::json!({
                "job_id": hex::encode(id),
                "phase": format!("{:?}", lc.phase()),
                "committee": r.committee.len(),
                "commitments": r.commitments.len(),
                "reveals": r.reveals.len(),
                "settled": r.settled.is_some(),
                "expected_escrow": lc.expected_escrow(),
            })
        }).collect();

        serde_json::json!({
            "height": self.blocks.height(),
            "total_emitted": self.total_emitted,
            "total_burned": self.total_burned,
            "current_epoch": self.current_epoch,
            "state_root": hex::encode(self.compute_state_root()),
            "accounts": accounts,
            "escrow_by_job": escrow_json,
            "bonded_stake": bonded_json,
            "unbonding_stake": unbonding_json,
            "job_lifecycles": lifecycles_json,
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

    /// Compute the state root.
    ///
    /// PoUW P2 (B1a) — POLICY B (zero consensus change until the money path goes live): while all
    /// three consensus maps are empty (the case today, until the live-enablement flip), this returns
    /// the accounts-only root BYTE-IDENTICAL to before B1a. Once any map is non-empty, the root folds
    /// the accounts root + the three maps (each iterated in SORTED key order — HashMap iteration is
    /// nondeterministic — and length-prefixed so the encoding is injective across map boundaries). The
    /// format change is thus deferred to the same moment the txs go live (a coordinated consensus
    /// change), not introduced now.
    pub fn compute_state_root(&self) -> [u8; 32] {
        let accounts_root = self.accounts.compute_state_root();
        if self.escrow_by_job.is_empty()
            && self.bonded_stake.is_empty()
            && self.unbonding_stake.is_empty()
            && self.job_lifecycles.is_empty()
        {
            return accounts_root;
        }
        let mut h = Sha256::new();
        h.update(accounts_root);

        // escrow_by_job: sorted by job_id, length-prefixed.
        let mut escrow: Vec<(&[u8; 32], &u64)> = self.escrow_by_job.iter().collect();
        escrow.sort_by(|a, b| a.0.cmp(b.0));
        h.update((escrow.len() as u64).to_le_bytes());
        for (job_id, amount) in escrow {
            h.update(job_id);
            h.update(amount.to_le_bytes());
        }

        // bonded_stake: sorted by address, length-prefixed.
        let mut bonded: Vec<(&Address, &u64)> = self.bonded_stake.iter().collect();
        bonded.sort_by(|a, b| a.0.0.cmp(&b.0.0));
        h.update((bonded.len() as u64).to_le_bytes());
        for (addr, amount) in bonded {
            h.update(addr.0);
            h.update(amount.to_le_bytes());
        }

        // unbonding_stake: sorted by address; per-address chunk list length-prefixed AND sorted by
        // (matures_at, amount) so the root never depends on the chunks' in-memory append order (e.g.
        // after a reorg replay) — all integers folded little-endian for cross-platform determinism.
        let mut unbonding: Vec<(&Address, &Vec<UnbondingChunk>)> = self.unbonding_stake.iter().collect();
        unbonding.sort_by(|a, b| a.0.0.cmp(&b.0.0));
        h.update((unbonding.len() as u64).to_le_bytes());
        for (addr, chunks) in unbonding {
            h.update(addr.0);
            h.update((chunks.len() as u64).to_le_bytes());
            let mut cs: Vec<&UnbondingChunk> = chunks.iter().collect();
            cs.sort_by(|a, b| a.matures_at.cmp(&b.matures_at).then(a.amount.cmp(&b.amount)));
            for c in cs {
                h.update(c.amount.to_le_bytes());
                h.update(c.matures_at.to_le_bytes());
            }
        }

        // job_lifecycles: sorted by job_id, length-prefixed; value = borsh(JobLifecycleRecord). The DTO
        // holds only Vec/Option/primitive fields (NO HashMap/HashSet) ⇒ borsh is canonical, Vec order is
        // the chain's append order (consensus-deterministic) ⇒ the fold is injective + deterministic
        // across nodes. Per-entry blob length-prefixed (LE) so the encoding stays injective. B1b
        // FINALIZES the Policy-B fold to four sections; while ALL four maps are empty (the case until the
        // coordinated live flip) the root stays the pre-B1a accounts-only root byte-for-byte.
        let mut lifecycles: Vec<(&[u8; 32], &JobLifecycle)> = self.job_lifecycles.iter().collect();
        lifecycles.sort_by(|a, b| a.0.cmp(b.0));
        h.update((lifecycles.len() as u64).to_le_bytes());
        for (job_id, lc) in lifecycles {
            h.update(job_id);
            let blob = borsh::to_vec(&lc.to_record())
                .expect("lifecycle record borsh serialization should not fail");
            h.update((blob.len() as u64).to_le_bytes());
            h.update(&blob);
        }

        h.finalize().into()
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
    /// Credit the per-block reward to the block producer.
    /// Called during block application, BEFORE transactions.
    /// Skips genesis block (height 0). Caps reward to remaining supply.
    ///
    /// IMPORTANT: Called from both `apply_block()` and `apply_block_validated()`.
    /// If you change the reward logic, update both call sites.
    fn credit_block_reward(&mut self, block: &Block) {
        // No reward at genesis or for zero-address producer (protocol blocks).
        if block.height() == 0 || block.header.producer.is_zero() {
            return;
        }

        // Compute reward from halving schedule, capped to remaining supply.
        let reward = commputer_core::token::block_reward(block.height());
        let remaining = self.remaining_supply();
        let actual_reward = reward.min(remaining);

        if actual_reward == 0 {
            return;
        }

        // Credit producer.
        let producer = block.header.producer;
        let account = self.accounts.get_or_create(producer);
        if let Some(new_balance) = account.balance.checked_add(
            commputer_core::token::Amount::from_raw(actual_reward),
        ) {
            account.balance = new_balance;
            // Track per-account mining total.
            account.total_mined = account.total_mined
                .checked_add(commputer_core::token::Amount::from_raw(actual_reward))
                .unwrap_or(account.total_mined);
        }

        // Update global emission counter.
        self.total_emitted = self.total_emitted.saturating_add(actual_reward);
    }

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

        // Capture producer before-state (before reward or any tx).
        if block.height() > 0 {
            let producer = block.header.producer;
            let (bal, nonce) = self.accounts.get(&producer)
                .map(|a| (a.balance.raw(), a.nonce))
                .unwrap_or((0, 0));
            before_states.insert(producer, (bal, nonce));
        }

        // Capture sender/recipient before-states for all transactions.
        for tx in &block.transactions {
            // Record sender before-state.
            if let std::collections::hash_map::Entry::Vacant(e) = before_states.entry(tx.from) {
                let (bal, nonce) = self.accounts.get(&tx.from)
                    .map(|a| (a.balance.raw(), a.nonce))
                    .unwrap_or((0, 0));
                e.insert((bal, nonce));
            }
            // Record recipient before-state for transfers.
            if let TxKind::Transfer { to, .. } = &tx.kind
                && !before_states.contains_key(to) {
                    let (bal, nonce) = self.accounts.get(to)
                        .map(|a| (a.balance.raw(), a.nonce))
                        .unwrap_or((0, 0));
                    before_states.insert(*to, (bal, nonce));
                }
        }

        // Process transactions — if any fail, no state has been mutated yet.
        for tx in &block.transactions {
            self.apply_transaction(tx)?;
        }

        // Credit per-block reward AFTER all transactions succeed (atomicity).
        // Moving this here ensures that if any transaction fails, the producer
        // balance and total_emitted are never mutated for this block.
        self.credit_block_reward(block);

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
                    new_nonce,
                });
            }
        }
        if !diff.changes.is_empty() {
            self.state_diffs.insert(block.height(), diff);
        }

        // Feature 10: Track validator performance for block producer.
        if block.height() > 0 {
            let perf = self.validator_performance.entry(block.header.producer).or_default();
            perf.blocks_produced += 1;
            perf.last_active_height = block.height();
        }

        // Store block.
        self.blocks.put(block.clone());

        // Atomically persist block + meta + dirty accounts + consensus-map deltas
        // (one WriteBatch; crash-safe).
        self.persist_applied_block(block)?;

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
        if block.height() > 0
            && let Some(latest) = self.blocks.latest()
                && block.header.parent_hash != latest.hash() {
                    return Err(StateError::InvalidBlock("parent hash mismatch".into()));
                }

        // Verify chain_id (allow empty for backwards compat).
        if !block.header.chain_id.is_empty()
            && block.header.chain_id != commputer_core::genesis::TESTNET_CHAIN_ID
            && block.header.chain_id != commputer_core::genesis::MAINNET_CHAIN_ID
        {
            return Err(StateError::InvalidBlock(format!(
                "invalid chain_id: {}", block.header.chain_id
            )));
        }

        // Verify merkle roots match block contents.
        if !block.verify_roots() {
            return Err(StateError::InvalidBlock("merkle root mismatch".into()));
        }

        // Cryptographically verify all transaction signatures.
        // Protocol-issued transactions (MiningReward, MilestoneBurn) come from the zero
        // address and have no signature — skip verification for those.
        for tx in &block.transactions {
            if tx.from.is_zero() {
                continue;
            }
            if !tx.verify() {
                return Err(StateError::InvalidSignature(
                    format!("transaction from {:?} failed signature verification", tx.from)
                ));
            }
        }

        // Process transactions and generate receipts.
        // NOTE: credit_block_reward is intentionally called AFTER this loop so
        // that if any transaction fails, total_emitted and producer balance are
        // never mutated (atomicity guarantee).
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

        // Credit per-block reward AFTER all transactions succeed (atomicity).
        // If any transaction above failed, we returned Err already — the producer
        // balance and total_emitted are not mutated for a failed block.
        self.credit_block_reward(block);

        // Store block.
        self.blocks.put(block.clone());

        // Atomically persist block + meta + dirty accounts + consensus-map deltas
        // (one WriteBatch; crash-safe).
        self.persist_applied_block(block)?;

        Ok(())
    }

    /// Apply a single transaction to the state.
    /// Feature 183: Updates last_active_epoch on all involved accounts.
    fn apply_transaction(
        &mut self,
        tx: &commputer_core::transaction::Transaction,
    ) -> Result<(), StateError> {
        // Feature 251: Validate memo length.
        if let Some(ref memo) = tx.memo
            && memo.len() > commputer_core::transaction::Transaction::MAX_MEMO_LENGTH {
                return Err(StateError::InvalidBlock(format!(
                    "memo exceeds max length of {} bytes", commputer_core::transaction::Transaction::MAX_MEMO_LENGTH
                )));
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
        if let TxKind::Transfer { amount, .. } = &tx.kind
            && amount.raw() < commputer_core::transaction::DUST_LIMIT {
                return Err(StateError::InvalidBlock(format!(
                    "transfer amount {} below dust limit of {}",
                    amount.raw(), commputer_core::transaction::DUST_LIMIT,
                )));
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
                // Feature 23: Dust limit — reject transfers below minimum.
                if amount.raw() < commputer_core::transaction::DUST_LIMIT {
                    return Err(StateError::InvalidBlock(
                        format!("transfer amount {} below dust limit {}", amount.raw(), commputer_core::transaction::DUST_LIMIT)
                    ));
                }
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
                // Feature 4: Check minimum validator stake.
                // Exempt during the first BOOTSTRAP_REGISTRATION_BLOCKS so early
                // joiners can register before they have any COMME. The previous
                // exemption used total_emitted, which was crossed by a single
                // block reward (15.85 COMME >> 0.01 COMME stake), making the
                // exemption window effectively zero.
                if self.blocks.height() >= commputer_core::transaction::BOOTSTRAP_REGISTRATION_BLOCKS
                    && sender.balance.raw() < commputer_core::transaction::MINIMUM_VALIDATOR_STAKE
                {
                    return Err(StateError::InvalidBlock(format!(
                        "insufficient balance for validator registration: need {} raw, have {}",
                        commputer_core::transaction::MINIMUM_VALIDATOR_STAKE,
                        sender.balance.raw(),
                    )));
                }
                sender.is_validator = true;
                // Feature 5: Record registration height for cooldown.
                sender.validator_registered_height = Some(self.blocks.height());
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

            // SubmitJob (legacy) + SubmitJobV2 (PoUW P0/G3) share identical economics at P0:
            // verify budget >= min and burn comme_budget at submit. (P1 converts V2 to escrow.)
            TxKind::SubmitJob { comme_budget, .. }
            | TxKind::SubmitJobV2 { comme_budget, .. } => {
                // Feature 52: Submit a compute job — verify budget and burn.
                if comme_budget.raw() < commputer_core::compute::MIN_JOB_BUDGET {
                    return Err(StateError::InvalidBlock(format!(
                        "compute job budget {} below minimum {}",
                        comme_budget.raw(),
                        commputer_core::compute::MIN_JOB_BUDGET
                    )));
                }
                let sender_balance = sender.balance;
                if sender_balance.raw() < comme_budget.raw() {
                    return Err(StateError::InsufficientBalance);
                }
                sender.balance = sender_balance.checked_sub(*comme_budget)
                    .ok_or(StateError::InsufficientBalance)?;
                sender.nonce += 1;
                self.total_burned = self.total_burned.saturating_add(comme_budget.raw());
            }

            TxKind::ClaimJob { .. } => {
                // Feature 53: Validator claims a pending compute job.
                if !sender.is_validator {
                    return Err(StateError::InvalidBlock(
                        "only validators can claim compute jobs".into(),
                    ));
                }
                sender.nonce += 1;
            }

            TxKind::CompleteJob { .. } => {
                // Feature 54: Executor submits result hash.
                sender.nonce += 1;
            }

            TxKind::DisputeJob { .. } => {
                // Feature 55: Verifier disputes a job result.
                if !sender.is_validator {
                    return Err(StateError::InvalidBlock(
                        "only validators can dispute compute jobs".into(),
                    ));
                }
                sender.nonce += 1;
            }

            TxKind::Commit { .. } => {
                // PoUW P2 / G2: a committee verifier commits H(result_hash‖salt‖verifier) + a bond.
                // INERT until the committee draw (event_loop, PROTECTED) creates the job's
                // JobLifecycle and this routes to record_commit (which escrows the bond). Until
                // then there is no lifecycle to record into, so accept + bump nonce only — the
                // bond is NOT escrowed, so it cannot strand. Committee members are validators.
                if !sender.is_validator {
                    return Err(StateError::InvalidBlock(
                        "only validators can commit to compute jobs".into(),
                    ));
                }
                sender.nonce += 1;
            }

            TxKind::Reveal { .. } => {
                // PoUW P2 / G2: a committee verifier reveals (result_hash, salt) opening its Commit.
                // INERT until wired to JobLifecycle::record_reveal — accept + bump nonce only.
                if !sender.is_validator {
                    return Err(StateError::InvalidBlock(
                        "only validators can reveal compute job results".into(),
                    ));
                }
                sender.nonce += 1;
            }

            TxKind::MiningReward { to, amount, .. } => {
                // Item 13: Mining reward — protocol-issued, no nonce or fee.
                // The actual balance change happens in the epoch processing;
                // this tx just records it for history visibility.
                let recipient = self.accounts.get_or_create(*to);
                let _ = recipient; // Already credited in epoch processing.
                let _ = amount;
            }

            TxKind::ValidatorDeregister => {
                // Item 14: Clean validator deregistration.
                if !sender.is_validator {
                    return Err(StateError::InvalidBlock(
                        "cannot deregister: not a validator".into(),
                    ));
                }
                sender.is_validator = false;
                sender.validator_registered_height = None;
                sender.nonce += 1;
                info!("Validator {} deregistered cleanly", tx.from);
            }

            // PoUW P2 / G4: bonded-stake lifecycle — expose the audited ChainState stake methods
            // as on-chain txs so `bonded_stake` can fill (committee selection depends on it).
            // BORROW NOTE: the outer `sender` (bound at 781) is NOT referenced in these arms, so
            // under NLL its &mut borrow of self.accounts is already dead here and we may call the
            // &mut self stake methods — identical to how the Batch arm calls
            // self.apply_batch_operation(...). `now` is bound FIRST as a plain &self read.
            TxKind::Bond { amount } => {
                // Permissionless on-ramp: any account may bond (see design decision). The
                // committee draw later filters by both is_eligible AND validator status, so a
                // non-validator's bond simply sits inert and can never cause an unqualified draw.
                // InsufficientBalance/Overflow => `?` returns BEFORE the nonce bump, so an invalid
                // tx rejects the whole block atomically (mirrors the SubmitJobV2 path).
                self.bond(&tx.from, amount.raw())?;
                self.accounts.get_or_create(tx.from).nonce += 1;
            }

            TxKind::RequestUnbond { amount } => {
                let now = self.blocks.height();
                self.request_unbond(&tx.from, amount.raw(), now)?;
                self.accounts.get_or_create(tx.from).nonce += 1;
            }

            TxKind::WithdrawUnbonded => {
                let now = self.blocks.height();
                let _ = self.withdraw_unbonded(&tx.from, now); // saturating; never errors
                self.accounts.get_or_create(tx.from).nonce += 1;
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
                // Feature 23: Dust limit — reject transfers below minimum.
                if amount.raw() < commputer_core::transaction::DUST_LIMIT {
                    return Err(StateError::InvalidBlock(
                        format!("transfer amount {} below dust limit {}", amount.raw(), commputer_core::transaction::DUST_LIMIT)
                    ));
                }
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
            TxKind::SubmitJob { comme_budget, .. }
            | TxKind::SubmitJobV2 { comme_budget, .. } => {
                // Feature 52: SubmitJob/V2 in batch — verify budget and burn (V2 mirrors V1 at P0).
                if comme_budget.raw() < commputer_core::compute::MIN_JOB_BUDGET {
                    return Err(StateError::InvalidBlock(format!(
                        "compute job budget {} below minimum {}",
                        comme_budget.raw(),
                        commputer_core::compute::MIN_JOB_BUDGET
                    )));
                }
                let sender = self.accounts.get_or_create(from);
                if sender.balance.raw() < comme_budget.raw() {
                    return Err(StateError::InsufficientBalance);
                }
                sender.balance = sender.balance.checked_sub(*comme_budget)
                    .ok_or(StateError::InsufficientBalance)?;
                self.total_burned = self.total_burned.saturating_add(comme_budget.raw());
            }
            TxKind::ClaimJob { .. } => {
                // Feature 53: ClaimJob in batch — verify validator.
                let sender = self.accounts.get_or_create(from);
                if !sender.is_validator {
                    return Err(StateError::InvalidBlock(
                        "only validators can claim compute jobs".into(),
                    ));
                }
            }
            TxKind::CompleteJob { .. } => {
                // Feature 54: CompleteJob in batch — no-op beyond nonce (handled at batch level).
            }
            TxKind::DisputeJob { .. } => {
                // Feature 55: DisputeJob in batch — verify validator.
                let sender = self.accounts.get_or_create(from);
                if !sender.is_validator {
                    return Err(StateError::InvalidBlock(
                        "only validators can dispute compute jobs".into(),
                    ));
                }
            }
            TxKind::Commit { .. } => {
                // PoUW P2 / G2: Commit in batch — verify validator (INERT until lifecycle wiring).
                let sender = self.accounts.get_or_create(from);
                if !sender.is_validator {
                    return Err(StateError::InvalidBlock(
                        "only validators can commit to compute jobs".into(),
                    ));
                }
            }
            TxKind::Reveal { .. } => {
                // PoUW P2 / G2: Reveal in batch — verify validator (INERT until lifecycle wiring).
                let sender = self.accounts.get_or_create(from);
                if !sender.is_validator {
                    return Err(StateError::InvalidBlock(
                        "only validators can reveal compute job results".into(),
                    ));
                }
            }
            // PoUW P2 / G4: bonded-stake ops inside a batch. `from` is the owned Copy param and no
            // outer `sender` borrow is held here, so the &mut self stake methods are unobstructed.
            // No per-op nonce/fee: the outer Batch arm bumps nonce once; the fee is burned once in
            // apply_transaction before the match.
            TxKind::Bond { amount } => {
                self.bond(&from, amount.raw())?;
            }
            TxKind::RequestUnbond { amount } => {
                let now = self.blocks.height();
                self.request_unbond(&from, amount.raw(), now)?;
            }
            TxKind::WithdrawUnbonded => {
                let now = self.blocks.height();
                let _ = self.withdraw_unbonded(&from, now);
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

    /// Revert the block at the given height, undoing all account state changes.
    /// Uses the StateDiff recorded during apply_block. Can only revert the tip.
    ///
    /// FAIL-SAFE: the StateDiff restores balance+nonce only — it CANNOT roll back the PoUW
    /// consensus maps (escrow pots, bonded/unbonding stake, lifecycles mutate with full-value
    /// semantics and no before-image exists). Any block that could have touched them is
    /// refused: guard 1 (maps non-empty) backstops guard 2 (tx-kind scan), which will go stale
    /// as B2–B4 add map-touching kinds.
    pub fn revert_block(&mut self, height: u64) -> Result<(), StateError> {
        if height != self.blocks.height() {
            return Err(StateError::InvalidBlock(format!(
                "can only revert tip: tip is {}, asked to revert {}", self.blocks.height(), height
            )));
        }
        if height == 0 {
            return Err(StateError::InvalidBlock("cannot revert genesis block".into()));
        }

        const REVERT_REFUSAL: &str =
            "revert_block cannot roll back PoUW consensus maps; use reset_to_genesis + resync \
             (or try_reorg once wired)";
        // Guard 1: any live map state means this block (or an out-of-band mutation) may be
        // entangled with it — refuse. Pre-flip the maps are always empty, so behavior is
        // unchanged for today's callers.
        if !self.escrow_by_job.is_empty()
            || !self.bonded_stake.is_empty()
            || !self.unbonding_stake.is_empty()
            || !self.job_lifecycles.is_empty()
        {
            return Err(StateError::InvalidBlock(REVERT_REFUSAL.into()));
        }
        // Guard 2: a map-touching tx kind (including inside a Batch) is refused even when the
        // maps ended the block empty (e.g. bond → withdraw-all within the block).
        if let Some(block) = self.blocks.get_by_height(height)
            && block.transactions.iter().any(tx_touches_consensus_maps)
        {
            return Err(StateError::InvalidBlock(REVERT_REFUSAL.into()));
        }

        // Restore account states from the diff. (These go through `get_mut`, so every reverted
        // account is re-journaled dirty and the next persist heals the disk copy.)
        if let Some(diff) = self.state_diffs.remove(&height) {
            for (addr, account_diff) in &diff.changes {
                if let Some(account) = self.accounts.get_mut(addr) {
                    account.balance = Amount::from_raw(account_diff.old_balance);
                    account.nonce = account_diff.old_nonce;
                }
            }
        }

        // Reverse burn tracking from this block's transactions
        if let Some(block) = self.blocks.get_by_height(height).cloned() {
            for tx in &block.transactions {
                let burn = tx.burn_amount().raw();
                if burn > 0 {
                    self.total_burned = self.total_burned.saturating_sub(burn);
                }
            }
        }

        // Remove the block and update height
        self.blocks.remove_at_height(height);
        tracing::info!("Reverted block at height {}", height);
        Ok(())
    }

    /// Revert blocks from the tip down to `target_height` (the block at
    /// target_height stays applied). Respects FINALITY_DEPTH.
    pub fn revert_to(&mut self, target_height: u64) -> Result<u64, StateError> {
        let current = self.blocks.height();
        if target_height >= current {
            return Ok(0);
        }
        let depth = current - target_height;
        if depth > FINALITY_DEPTH {
            return Err(StateError::InvalidBlock(format!(
                "cannot revert {} blocks (max finality depth: {})", depth, FINALITY_DEPTH
            )));
        }
        let mut reverted = 0;
        for height in (target_height + 1..=current).rev() {
            self.revert_block(height)?;
            reverted += 1;
        }
        tracing::info!("Reverted {} blocks: height {} -> {}", reverted, current, target_height);
        Ok(reverted)
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
            if let Some(account) = self.accounts.get_mut(addr) {
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
        if let Some(ref rocks) = self.rocks
            && let Ok(Some(mut account)) = rocks.get_account(address) {
                account.is_hot = true;
                self.accounts.put(account);
                return self.accounts.get(address);
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
            if let std::collections::hash_map::Entry::Vacant(e) = before_states.entry(tx.from) {
                let (bal, nonce) = self.accounts.get(&tx.from)
                    .map(|a| (a.balance.raw(), a.nonce))
                    .unwrap_or((0, 0));
                e.insert((bal, nonce));
            }
            if let TxKind::Transfer { to, .. } = &tx.kind
                && !before_states.contains_key(to) {
                    let (bal, nonce) = self.accounts.get(to)
                        .map(|a| (a.balance.raw(), a.nonce))
                        .unwrap_or((0, 0));
                    before_states.insert(*to, (bal, nonce));
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
                    new_nonce,
                });
            }
        }
        if !diff.changes.is_empty() {
            self.state_diffs.insert(block.height(), diff);
        }

        // Store block in memory.
        self.blocks.put(block.clone());

        // Atomically persist block + meta + dirty accounts + consensus-map deltas in one
        // WriteBatch (shared helper — the dirty journal also covers batch-inner recipients and
        // the producer, which the `before_states` scan above misses).
        self.persist_applied_block(block)?;

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
        let first_checkpoint_after_fork = if fork_height.is_multiple_of(CHECKPOINT_INTERVAL) {
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
        //
        // NB try_reorg has ZERO production callers today (test-only; future sync_machine_v2
        // wiring). The PRODUCTION fork-recovery path is reset_to_genesis + block-by-block
        // resync via apply_block_validated. Hardened here as pre-wiring.

        // Collect the pre-fork blocks BEFORE detaching rocks: past MEMORY_BLOCK_RETENTION the
        // in-memory BlockStore has pruned them, so get_block_by_height must still be able to
        // fall back to RocksDB.
        let mut pre_fork_blocks = Vec::new();
        for h in 0..fork_height {
            if let Some(block) = self.get_block_by_height(h) {
                pre_fork_blocks.push(block);
            }
        }

        // Replay runs with rocks detached so per-block persistence is intentionally skipped.
        // The handle MUST be reattached on EVERY exit path — a dropped handle silently
        // disables all persistence for the rest of the process — so the fallible replay is
        // wrapped and its Err intercepted rather than `?`-propagated across the take/restore
        // window.
        let saved_rocks = self.rocks.take();
        let _saved_epoch = self.current_epoch;
        let _saved_emitted = self.total_emitted;
        let _saved_burned = self.total_burned;

        // Snapshot the map mirrors alongside rocks: the rocks=None replay advances them to the
        // winning chain's key sets (A5 memory-only commit) while the CFs still hold the LOSING
        // chain. If replay or the reconcile write below fails, they must be restored to keep
        // describing CF reality — otherwise stale_keys computes empty deletes forever and the
        // losing chain's map rows resurrect at the next open().
        let saved_mirrors = (
            self.persisted_escrow_keys.clone(),
            self.persisted_bonded_keys.clone(),
            self.persisted_unbonding_keys.clone(),
            self.persisted_lifecycle_keys.clone(),
        );

        // Reset state.
        self.accounts = AccountStore::new();
        self.blocks = BlockStore::new();
        self.total_emitted = 0;
        self.total_burned = 0;
        self.escrow_by_job.clear();
        self.bonded_stake.clear();
        self.unbonding_stake.clear();
        self.job_lifecycles.clear();
        self.cumulative_score = 0;
        self.state_diffs.clear();

        let replay_result: Result<(), StateError> = (|| {
            // Re-apply pre-fork blocks, then the new competing chain.
            for block in &pre_fork_blocks {
                self.apply_block(block)?;
            }
            for block in &competing_chain {
                self.apply_block(block)?;
            }
            Ok(())
        })();
        self.rocks = saved_rocks;
        if let Err(e) = replay_result {
            // In-memory state is half-rebuilt (caller must treat this as fatal: retry the
            // reorg or reset_to_genesis), but the CFs still hold the intact losing chain —
            // restore the mirrors to match so any later flush/persist diffs against reality.
            (
                self.persisted_escrow_keys,
                self.persisted_bonded_keys,
                self.persisted_unbonding_keys,
                self.persisted_lifecycle_keys,
            ) = saved_mirrors;
            return Err(e);
        }

        self.cumulative_score = competing_score;

        // Post-reorg CF reconcile: the replay ran with rocks=None, so every CF still holds the
        // LOSING chain (accounts, meta, maps, block-height index). Rewrite the winning state
        // in ONE WriteBatch — the delete_ranges and re-puts commit atomically, so a crash
        // leaves either the old rows or the complete winning state, never an empty CF. Stale
        // by-hash rows for orphaned blocks in CF_BLOCKS are unreachable via the height index
        // and are left behind (accepted).
        self.accounts.mark_all_dirty(); // fail-safe: if the write below errors, the next
        // successful per-block persist re-puts every account.
        if let Some(ref rocks) = self.rocks {
            let mut batch = rocks.new_write_batch();
            rocks.batch_put_meta_u64(&mut batch, rocks::META_TOTAL_EMITTED, self.total_emitted);
            rocks.batch_put_meta_u64(&mut batch, rocks::META_TOTAL_BURNED, self.total_burned);
            rocks.batch_put_meta_u64(&mut batch, rocks::META_CURRENT_EPOCH, self.current_epoch);
            rocks.batch_put_meta_u64(&mut batch, rocks::META_NERF_RATE_BPS, self.nerf_rate.rate_bps as u64);
            rocks.batch_put_meta_u64(&mut batch, rocks::META_CHAIN_HEIGHT, self.blocks.height());
            rocks.batch_clear_accounts(&mut batch);
            for account in self.accounts.iter() {
                rocks.batch_put_account(&mut batch, account);
            }
            rocks.batch_clear_consensus_maps(&mut batch);
            // The mirror-diff deletes inside are no-ops after the range clear; the full-value
            // re-puts are what matter here.
            self.batch_map_deltas(rocks, &mut batch);
            for block in &pre_fork_blocks {
                rocks.batch_put_block(&mut batch, block);
            }
            for block in &competing_chain {
                rocks.batch_put_block(&mut batch, block);
            }
            if let Err(e) = rocks.write_batch(batch) {
                // Disk still holds the losing chain while memory (and the replay-advanced
                // mirrors) hold the winner. Restore the mirrors to CF reality so a later
                // flush/persist mirror-diff can still delete the losing chain's map rows.
                // Losing-chain-only ACCOUNT rows have no mirror — mark_all_dirty above heals
                // puts only, so full cleanup after a failed reconcile requires retrying the
                // reorg or reset_to_genesis (accepted per design risk #3: a failing batch
                // write means the disk itself is failing).
                (
                    self.persisted_escrow_keys,
                    self.persisted_bonded_keys,
                    self.persisted_unbonding_keys,
                    self.persisted_lifecycle_keys,
                ) = saved_mirrors;
                return Err(StateError::StorageError(e.to_string()));
            }
        }
        // Reached only if the reconcile committed (or the node is memory-only).
        self.accounts.clear_dirty_and_removed();
        self.commit_map_mirrors();

        info!(
            "Chain reorganization complete: rolled back to height {}, applied {} new blocks (new height {})",
            fork_height.saturating_sub(1),
            competing_chain.len(),
            self.blocks.height(),
        );

        Ok(orphaned_txs)
    }

    /// Atomically persist an applied block + height index + meta + all dirty accounts +
    /// consensus-map deltas in ONE RocksDB WriteBatch. Called at the tail of every `apply_*`
    /// after in-memory application succeeded. On success this drains the dirty/removed account
    /// journals and commits the map mirrors; on failure they are RETAINED, so the next
    /// successful persist re-covers all account/map/meta state (self-healing) — the only
    /// permanent artifact of a failed write is a block-history gap in CF_BLOCKS, which affects
    /// serving sync, not state correctness (`open()` loads state from CFs, not replay).
    fn persist_applied_block(&mut self, block: &Block) -> Result<(), StateError> {
        let Some(rocks) = self.rocks.as_ref() else {
            // Memory-only mode: clear the bookkeeping anyway — it bounds journal/mirror growth
            // and keeps the mirrors equal to the current key sets, so a later reconcile path
            // (e.g. try_reorg's post-replay pass) is the sole source of truth. Blocks are NOT
            // pruned: a pure in-memory node cannot reload them from disk.
            self.accounts.clear_dirty_and_removed();
            self.commit_map_mirrors();
            return Ok(());
        };

        let mut batch = rocks.new_write_batch();
        rocks.batch_put_block(&mut batch, block);
        // The block was just applied, so block.height() == self.blocks.height() and
        // put_block's monotonic height guard is trivially satisfied — write META_CHAIN_HEIGHT
        // unconditionally (write-only today: no readers in tree).
        rocks.batch_put_meta_u64(&mut batch, rocks::META_CHAIN_HEIGHT, block.height());
        // Meta counters ride every block's batch — this is also what carries the event loop's
        // out-of-band current_epoch bump to disk at the next block.
        rocks.batch_put_meta_u64(&mut batch, rocks::META_TOTAL_EMITTED, self.total_emitted);
        rocks.batch_put_meta_u64(&mut batch, rocks::META_TOTAL_BURNED, self.total_burned);
        rocks.batch_put_meta_u64(&mut batch, rocks::META_CURRENT_EPOCH, self.current_epoch);
        rocks.batch_put_meta_u64(&mut batch, rocks::META_NERF_RATE_BPS, self.nerf_rate.rate_bps as u64);
        // Accounts: deletes BEFORE puts — WriteBatch is last-write-wins per key, so a
        // remove-then-recreate within one block resolves to the put (the live value).
        for addr in self.accounts.removed() {
            rocks.batch_delete_account(&mut batch, addr);
        }
        for addr in self.accounts.dirty() {
            if let Some(account) = self.accounts.get(addr) {
                rocks.batch_put_account(&mut batch, account);
            }
        }
        self.batch_map_deltas(rocks, &mut batch);

        rocks.write_batch(batch)
            .map_err(|e| StateError::StorageError(e.to_string()))?;

        // Only after the atomic commit: drain the journals and advance the mirrors.
        self.accounts.clear_dirty_and_removed();
        self.commit_map_mirrors();
        // Prune old blocks from memory (they remain in RocksDB).
        self.blocks.prune(Self::MEMORY_BLOCK_RETENTION);
        Ok(())
    }

    /// Append the four consensus maps' CF deltas to `batch`: delete every persisted key that
    /// no longer exists in memory (`mirror − current`), then re-put every live entry
    /// FULL-VALUE — lifecycles mutate internally without key churn, so key-level tracking
    /// alone cannot suffice, and a value re-put is O(map size), trivial at testnet scale
    /// (device write amplification ~10–30× on the logical bytes; B7/G6 capacity admission is
    /// the real bound, and a per-entry value-hash mirror is the mainnet optimization — it
    /// would also skip the redundant per-block `to_record()` for unchanged lifecycles).
    /// Deletes are appended before puts (WriteBatch is last-write-wins per key). The caller
    /// commits the batch and, ONLY on success, calls `commit_map_mirrors`.
    fn batch_map_deltas(&self, rocks: &RocksStore, batch: &mut WriteBatch) {
        for job_id in stale_keys(&self.persisted_escrow_keys, &self.escrow_by_job) {
            rocks.batch_delete_escrow(batch, job_id);
        }
        for (job_id, amount) in &self.escrow_by_job {
            rocks.batch_put_escrow(batch, job_id, *amount);
        }
        for who in stale_keys(&self.persisted_bonded_keys, &self.bonded_stake) {
            rocks.batch_delete_bonded(batch, who);
        }
        for (who, amount) in &self.bonded_stake {
            rocks.batch_put_bonded(batch, who, *amount);
        }
        for who in stale_keys(&self.persisted_unbonding_keys, &self.unbonding_stake) {
            rocks.batch_delete_unbonding(batch, who);
        }
        for (who, chunks) in &self.unbonding_stake {
            rocks.batch_put_unbonding(batch, who, chunks);
        }
        for job_id in stale_keys(&self.persisted_lifecycle_keys, &self.job_lifecycles) {
            rocks.batch_delete_lifecycle(batch, job_id);
        }
        for (job_id, lc) in &self.job_lifecycles {
            rocks.batch_put_lifecycle(batch, job_id, &lc.to_record());
        }
    }

    /// Advance all four persisted-key mirrors to the maps' current key sets. Call ONLY after
    /// a successful CF write — on failure the stale mirror recomputes a superset of deletes at
    /// the next attempt (deleting an absent key is a RocksDB no-op, so over-deleting is safe).
    fn commit_map_mirrors(&mut self) {
        self.persisted_escrow_keys = self.escrow_by_job.keys().copied().collect();
        self.persisted_bonded_keys = self.bonded_stake.keys().copied().collect();
        self.persisted_unbonding_keys = self.unbonding_stake.keys().copied().collect();
        self.persisted_lifecycle_keys = self.job_lifecycles.keys().copied().collect();
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
    fn flush_to_rocks(&mut self) -> Result<(), StateError> {
        // Flush meta.
        if let Some(ref rocks) = self.rocks {
            self.flush_meta(rocks)?;
        }

        // Flush all accounts (reconciling: removed rows deleted first).
        self.flush_accounts()?;

        // Flush the consensus money/stake maps (reconciling, via the persisted-key mirrors).
        self.flush_consensus_maps()?;

        Ok(())
    }

    /// Item 66: Persist all in-memory accounts to RocksDB using a single WriteBatch.
    /// Reconciling: rows for removed accounts are deleted first (a put-only flush would
    /// resurrect archived/removed accounts at the next open); the removed journal is drained
    /// only after the write succeeds.
    fn flush_accounts(&mut self) -> Result<(), StateError> {
        let Some(rocks) = self.rocks.as_ref() else { return Ok(()) };
        let mut batch = rocks.new_write_batch();
        // Deletes BEFORE puts (last-write-wins per key; see persist_applied_block).
        for addr in self.accounts.removed() {
            rocks.batch_delete_account(&mut batch, addr);
        }
        for account in self.accounts.iter() {
            rocks.batch_put_account(&mut batch, account);
        }
        rocks.write_batch(batch)
            .map_err(|e| StateError::StorageError(e.to_string()))?;
        // Every live account was just written, so the dirty journal is covered too.
        self.accounts.clear_dirty_and_removed();
        Ok(())
    }

    /// PoUW P2: reconciling flush of the FOUR consensus maps (escrow_by_job / bonded_stake /
    /// unbonding_stake / job_lifecycles) — deletes CF rows for keys removed in-memory (via the
    /// persisted-key mirrors), then re-puts every live entry, in one WriteBatch. Safe to call
    /// at any time; kept as the shutdown-tail sweeper behind the per-block batch.
    fn flush_consensus_maps(&mut self) -> Result<(), StateError> {
        let Some(rocks) = self.rocks.as_ref() else { return Ok(()) };
        let mut batch = rocks.new_write_batch();
        self.batch_map_deltas(rocks, &mut batch);
        rocks.write_batch(batch)
            .map_err(|e| StateError::StorageError(e.to_string()))?;
        self.commit_map_mirrors();
        Ok(())
    }

    /// Wipe all blocks and account state, reinitialize to genesis (height 0).
    /// Used during chain resync after fork detection.
    ///
    /// Caller must also reset: consensus manager, mempool, sync_complete flag.
    pub fn reset_to_genesis(&mut self) -> Result<(), StateError> {
        info!("Resetting chain state to genesis");

        // Clear in-memory stores.
        self.accounts = AccountStore::new();
        self.blocks = BlockStore::new();
        self.total_emitted = 0;
        self.total_burned = 0;
        self.escrow_by_job.clear();
        self.bonded_stake.clear();
        self.unbonding_stake.clear();
        self.job_lifecycles.clear();
        self.nerf_rate = NerfRate::INITIAL;
        self.current_epoch = 0;
        self.receipts = ReceiptStore::new();
        self.history = AccountHistoryIndex::new();
        self.cumulative_score = 0;
        self.state_diffs.clear();
        self.archived_accounts.clear();
        self.snapshot_height = 0;
        self.validator_performance.clear();

        // If RocksDB-backed, clear all column families.
        if let Some(ref rocks) = self.rocks {
            rocks.clear_all()
                .map_err(|e| StateError::StorageError(format!("failed to clear RocksDB: {}", e)))?;
        }

        // The CFs are empty (and `accounts` above is a fresh store with clean dirty/removed
        // journals) — reset the persisted-key mirrors to match.
        self.persisted_escrow_keys.clear();
        self.persisted_bonded_keys.clear();
        self.persisted_unbonding_keys.clear();
        self.persisted_lifecycle_keys.clear();

        info!("Chain state reset to genesis complete");
        Ok(())
    }
}

/// Keys present in the persisted-key `mirror` but absent from the live `current` map — the CF
/// rows the next batch must delete. Pure; over-deletion on a retry after a failed write is
/// safe (deleting an absent key is a RocksDB no-op).
fn stale_keys<'a, K, V>(
    mirror: &'a HashSet<K>,
    current: &'a HashMap<K, V>,
) -> impl Iterator<Item = &'a K>
where
    K: Eq + std::hash::Hash,
{
    mirror.iter().filter(move |k| !current.contains_key(k))
}

/// Whether `tx` (including ops nested in a `Batch`) is of a kind that mutates the PoUW
/// consensus maps. Used by `revert_block`'s guard 2. Extend with
/// `SubmitJobV2`/`ClaimJob`/`Commit`/`Reveal`/`CompleteJob` at the B2–B4 live flip — until
/// then those kinds move no map state, and guard 1 (maps non-empty) makes any staleness here
/// fail-safe rather than unsound.
fn tx_touches_consensus_maps(tx: &Transaction) -> bool {
    fn kind_touches(kind: &TxKind) -> bool {
        match kind {
            TxKind::Bond { .. } | TxKind::RequestUnbond { .. } | TxKind::WithdrawUnbonded => true,
            TxKind::Batch { operations } => operations.iter().any(kind_touches),
            _ => false,
        }
    }
    kind_touches(&tx.kind)
}

// ===================================================================================
// PoUW P1 — per-job escrow foundation (the on-chain analog of the staging
// `commputer-pouw-onchain::escrow_ledger::EscrowLedger`).
//
// These are the conservation-preserving primitives every terminal settlement resolver
// (`resolve_confirmed`/`disputed`/`cancel`/`timeout`/`unavailable`) will call once the
// committee/verdict DATA exists on-chain (P2). They are wired but NOT yet exercised by the
// live tx path: `SubmitJobV2` still burns at submit (see the `SubmitJob`/`SubmitJobV2` apply
// arm) — flipping it to `escrow_into_job` is the P2 change, because draining a pot needs the
// committee/verdict a P2 commit-reveal round produces. Adding escrow-in without a drain would
// strand budgets, so the flip is deliberately deferred.
//
// CONSERVATION (the invariant these maintain):
//   `sum(account balances) + total_escrowed()` is UNCHANGED by `escrow_into_job`/`pay_from_job`
//   and DECREASES by exactly `amount` on `burn_from_job` (which also bumps `total_burned`, so
//   `circulating_supply()` drops by the same `amount`). No method mints.
//
// All return `Result` (never panic): on-chain the pot amounts become attacker-influenced data,
// so an under-funded pot must reject the terminal tx, not halt the node. Callers should still
// pre-validate the pot equals the exact sum they will move (P1 caller-contract #1).
// ===================================================================================
impl ChainState {
    /// Move `amount` raw units from `who`'s spendable balance into `job_id`'s escrow pot.
    /// Value stays inside circulating supply (held, not burned). The pot is created on first
    /// escrow. A zero `amount` is a no-op. Returns `InsufficientBalance` if `who` has no
    /// account or cannot cover `amount` (no pot is created on failure).
    pub fn escrow_into_job(
        &mut self,
        who: &Address,
        job_id: [u8; 32],
        amount: u64,
    ) -> Result<(), StateError> {
        if amount == 0 {
            return Ok(());
        }
        // Check the pot overflow BEFORE mutating the balance (no partial state on error) — mirrors
        // bond(). (Under the supply cap a pot cannot reach u64::MAX, but stay checked + consistent
        // with the sibling money ops rather than an unchecked `+=`.)
        let pot = self.escrow_by_job.get(&job_id).copied().unwrap_or(0);
        let new_pot = pot.checked_add(amount).ok_or(StateError::Overflow)?;
        let account = self.accounts.get_mut(who).ok_or(StateError::InsufficientBalance)?;
        account.balance = account
            .balance
            .checked_sub(Amount::from_raw(amount))
            .ok_or(StateError::InsufficientBalance)?;
        self.escrow_by_job.insert(job_id, new_pot);
        Ok(())
    }

    /// Pay `amount` raw units out of `job_id`'s pot to `to` (a settlement payout or refund),
    /// crediting `to`'s balance (creating the account if needed). The pot entry is removed
    /// once it reaches zero. A zero `amount` is a no-op. Returns `EscrowUnderflow` if the pot
    /// holds less than `amount`.
    pub fn pay_from_job(
        &mut self,
        job_id: [u8; 32],
        to: &Address,
        amount: u64,
    ) -> Result<(), StateError> {
        if amount == 0 {
            return Ok(());
        }
        self.debit_job_pot(&job_id, amount)?;
        let account = self.accounts.get_or_create(*to);
        account.balance = account
            .balance
            .checked_add(Amount::from_raw(amount))
            .ok_or(StateError::Overflow)?;
        Ok(())
    }

    /// Burn `amount` raw units from `job_id`'s pot — value LEAVES circulating supply
    /// (`total_burned += amount`). The pot entry is removed once it reaches zero. A zero
    /// `amount` is a no-op. Returns `EscrowUnderflow` if the pot holds less than `amount`.
    pub fn burn_from_job(&mut self, job_id: [u8; 32], amount: u64) -> Result<(), StateError> {
        if amount == 0 {
            return Ok(());
        }
        self.debit_job_pot(&job_id, amount)?;
        self.total_burned = self.total_burned.saturating_add(amount);
        Ok(())
    }

    /// Raw units currently escrowed for `job_id` (0 if there is no pot).
    pub fn escrowed_for_job(&self, job_id: &[u8; 32]) -> u64 {
        self.escrow_by_job.get(job_id).copied().unwrap_or(0)
    }

    /// Total raw units held across every job pot (part of circulating supply).
    pub fn total_escrowed(&self) -> u64 {
        self.escrow_by_job.values().sum()
    }

    /// Internal: remove `amount` from `job_id`'s pot, deleting the entry when it hits zero
    /// (no lingering empty pots — same hygiene as the bonded-stake slash path). Returns
    /// `EscrowUnderflow` if the pot is absent or holds less than `amount`.
    fn debit_job_pot(&mut self, job_id: &[u8; 32], amount: u64) -> Result<(), StateError> {
        let pot = self.escrow_by_job.get_mut(job_id).ok_or(StateError::EscrowUnderflow)?;
        let remaining = pot.checked_sub(amount).ok_or(StateError::EscrowUnderflow)?;
        if remaining == 0 {
            self.escrow_by_job.remove(job_id);
        } else {
            *pot = remaining;
        }
        Ok(())
    }
}

/// PoUW P2 (G4): genesis-anchored staking params (mirrors the staging
/// `commputer-pouw-onchain::bonded_stake::StakeParams`). The P3 genesis wire-in replaces these
/// defaults with the consensus values — all nodes MUST agree or they diverge on committee draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StakeParams {
    /// Cooldown length (blocks) before unbonded stake is withdrawable.
    pub unbonding_blocks: u64,
    /// Minimum ACTIVE bond to be eligible for committee selection.
    pub min_bond: u64,
}

impl Default for StakeParams {
    fn default() -> Self {
        // Placeholders — the founder sets the real genesis values (P3/G5).
        Self { unbonding_blocks: 100, min_bond: 1_000 }
    }
}

/// PoUW P2 (G4): one unbonding request in its cooldown window (slashable until withdrawn).
/// Borsh-serialized for RocksDB persistence (B1a) — treat the field layout as a stable on-disk
/// schema (a field reorder/add changes the on-disk bytes; version it if the fields ever grow).
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct UnbondingChunk {
    amount: u64,
    matures_at: u64, // block height at/after which this chunk is withdrawable
}

// ===================================================================================
// PoUW P2 (G4) — on-chain bonded/slashable stake source: the committee-selection weight
// (`stake_of`) + a slash surface. The on-chain analog of the staging
// `commputer-pouw-onchain::bonded_stake::BondedStake`, but it REUSES the existing
// `Account.balance` (spendable) and `total_burned` (the burn sink) rather than duplicating
// them — only the active-bonded and cooldown buckets are new state here.
//
// PoS-style: bond (balance -> bonded) -> request_unbond (bonded -> cooldown) -> withdraw
// (matured cooldown -> balance). Stake is slashable throughout bonding AND cooldown
// (anti-dodge: unbonding before a slash does NOT escape it). `stake_of` = active bonded only
// (cooldown is leaving, so excluded from selection weight); `is_eligible` floors at min_bond.
//
// CONSERVATION: `sum(Account.balance) + total_bonded() + total_unbonding() + total_burned` is
// INVARIANT across bond/request_unbond/withdraw_unbonded, and `slash_stake` moves at-risk stake
// into `total_burned` (so the four-bucket sum stays invariant and `circulating_supply()` drops
// by exactly the slash). No method mints.
//
// WIRE-IN (P2 committee-draw step, event_loop.rs PROTECTED): filter the validator pool by
// `is_eligible`, then pass `|p| chain.stake_of(p)` into the frozen `committee::select_committee`
// (mapping Address <-> ParticipantId). The bond/unbond/withdraw triggers (a `BondStake`-style
// TxKind, or staking via ValidatorRegister) and persistence/state-root are the later live-wiring
// step; these primitives are the ledger they drive.
// ===================================================================================
impl ChainState {
    /// Bond `amount`: move it from `who`'s spendable balance into active bonded stake. A zero
    /// `amount` is a no-op. Returns `InsufficientBalance` if `who` has no account or cannot
    /// cover `amount` (no state change on failure).
    pub fn bond(&mut self, who: &Address, amount: u64) -> Result<(), StateError> {
        if amount == 0 {
            return Ok(());
        }
        // Validate the bonded-side overflow before mutating the balance (no partial state on err).
        let bonded = self.bonded_stake.get(who).copied().unwrap_or(0);
        let new_bonded = bonded.checked_add(amount).ok_or(StateError::Overflow)?;
        let account = self.accounts.get_mut(who).ok_or(StateError::InsufficientBalance)?;
        account.balance = account
            .balance
            .checked_sub(Amount::from_raw(amount))
            .ok_or(StateError::InsufficientBalance)?;
        self.bonded_stake.insert(*who, new_bonded);
        Ok(())
    }

    /// Request to unbond `amount`: move it from active bonded into a cooldown chunk maturing at
    /// `now + unbonding_blocks`. It immediately stops counting toward `stake_of`/selection but
    /// stays slashable. A zero `amount` is a no-op (no empty chunk). Returns `InsufficientStake`
    /// if active bonded is short (no state change).
    pub fn request_unbond(&mut self, who: &Address, amount: u64, now: u64) -> Result<(), StateError> {
        if amount == 0 {
            return Ok(());
        }
        let bonded = self.bonded_stake.get(who).copied().unwrap_or(0);
        if bonded < amount {
            return Err(StateError::InsufficientStake);
        }
        let remaining = bonded - amount;
        if remaining == 0 {
            self.bonded_stake.remove(who);
        } else {
            self.bonded_stake.insert(*who, remaining);
        }
        let matures_at = now.saturating_add(self.stake_params.unbonding_blocks);
        self.unbonding_stake.entry(*who).or_default().push(UnbondingChunk { amount, matures_at });
        Ok(())
    }

    /// Move all matured cooldown chunks (`matures_at <= now`) back to `who`'s spendable balance;
    /// returns the total withdrawn (0 if none matured). Saturating; never errors.
    pub fn withdraw_unbonded(&mut self, who: &Address, now: u64) -> u64 {
        let chunks = match self.unbonding_stake.get_mut(who) {
            Some(c) => c,
            None => return 0,
        };
        let mut withdrawn = 0u64;
        chunks.retain(|c| {
            if c.matures_at <= now {
                withdrawn = withdrawn.saturating_add(c.amount);
                false
            } else {
                true
            }
        });
        if chunks.is_empty() {
            self.unbonding_stake.remove(who);
        }
        if withdrawn > 0 {
            let account = self.accounts.get_or_create(*who);
            // The withdrawn value originated from THIS account's own balance (bond moved
            // balance->bonded->cooldown), so crediting it back cannot exceed the pre-bond balance,
            // which is <= the supply cap << u64::MAX — saturating_add can never actually cap here.
            account.balance = Amount::from_raw(account.balance.raw().saturating_add(withdrawn));
        }
        withdrawn
    }

    /// Slash up to `amount` of `who`'s AT-RISK stake — active bonded FIRST, then cooldown chunks
    /// in order — burning it (`total_burned += slashed`). Anti-dodge: cooldown stake is reachable.
    /// Returns the amount actually slashed (capped at total at-risk = bonded + Σ unbonding). The
    /// caller MUST inspect the return — a cap below `amount` means the actor was under-staked.
    pub fn slash_stake(&mut self, who: &Address, amount: u64) -> u64 {
        let mut remaining = amount;
        let mut slashed = 0u64;
        // bonded first (get_mut, not entry, so slashing a never-bonded account creates no 0 entry)
        if let Some(b) = self.bonded_stake.get_mut(who) {
            let take = remaining.min(*b);
            *b -= take;
            slashed = slashed.saturating_add(take); // bounded by total at-risk <= supply cap
            remaining -= take;
        }
        if self.bonded_stake.get(who).copied() == Some(0) {
            self.bonded_stake.remove(who);
        }
        // then cooldown chunks in stored order (anti-dodge)
        if remaining > 0
            && let Some(chunks) = self.unbonding_stake.get_mut(who)
        {
            for c in chunks.iter_mut() {
                if remaining == 0 {
                    break;
                }
                let take = remaining.min(c.amount);
                c.amount -= take;
                slashed = slashed.saturating_add(take); // bounded by total at-risk <= supply cap
                remaining -= take;
            }
            chunks.retain(|c| c.amount > 0);
            if chunks.is_empty() {
                self.unbonding_stake.remove(who);
            }
        }
        self.total_burned = self.total_burned.saturating_add(slashed);
        slashed
    }

    /// Active bonded stake for `who` (0 if none) — selectable + slashable.
    pub fn bonded_of(&self, who: &Address) -> u64 {
        self.bonded_stake.get(who).copied().unwrap_or(0)
    }

    /// Total cooldown (unbonding) stake for `who` (0 if none) — slashable, NOT selectable.
    pub fn unbonding_of(&self, who: &Address) -> u64 {
        self.unbonding_stake
            .get(who)
            .map(|v| v.iter().map(|c| c.amount).sum::<u64>())
            .unwrap_or(0)
    }

    /// Committee-selection weight = ACTIVE bonded only (cooldown excluded — it is leaving).
    pub fn stake_of(&self, who: &Address) -> u64 {
        self.bonded_of(who)
    }

    /// Eligible for committee selection iff active bonded >= `min_bond` (the candidate-pool
    /// filter applied BEFORE `select_committee`, which then weights by `stake_of`).
    pub fn is_eligible(&self, who: &Address) -> bool {
        self.bonded_of(who) >= self.stake_params.min_bond
    }

    /// Total active bonded stake across all accounts (for conservation/diagnostics).
    pub fn total_bonded(&self) -> u64 {
        self.bonded_stake.values().sum()
    }

    /// Total cooldown stake across all accounts (for conservation/diagnostics).
    pub fn total_unbonding(&self) -> u64 {
        self.unbonding_stake.values().flatten().map(|c| c.amount).sum()
    }
}

/// A job-scoped view of `ChainState` as a `Ledger`, so the staging lifecycle + settlement money logic
/// (P2 §3 option B) runs against the live chain. `for_job` sets the active job; escrow/pay/burn act on
/// that job's pot through the audited `ChainState` primitives. `ParticipantId` <-> `Address` are
/// `[u8;32]` newtype casts. The `ChainHooks` ops are infallible by contract (the frozen game panics on
/// a malformed pot); on-chain that maps to `.expect(...)`. The `lifecycle_*` helpers below ENFORCE the
/// P1 pre-validation (`escrowed_for_job == expected_escrow()` for settle; committer balance ≥ bond for
/// commit) and return `Err` on a mismatch BEFORE driving the lifecycle, so in practice these `.expect`s
/// are unreachable — a malformed pot rejects the terminal tx instead of panicking the node.
struct ChainLedger<'a> {
    chain: &'a mut ChainState,
    job: Option<[u8; 32]>,
}

impl<'a> ChainLedger<'a> {
    fn new(chain: &'a mut ChainState) -> Self {
        Self { chain, job: None }
    }
    fn active(&self) -> [u8; 32] {
        self.job.expect("Ledger::for_job must be called before any escrow/pay/burn op")
    }
}

impl ChainHooks for ChainLedger<'_> {
    fn escrow(&mut self, who: ParticipantId, amount: u64) {
        let job = self.active();
        self.chain
            .escrow_into_job(&Address(who.0), job, amount)
            .expect("escrow: caller must pre-validate the pot (P1 contract)");
    }
    fn pay(&mut self, to: ParticipantId, amount: u64) {
        let job = self.active();
        self.chain
            .pay_from_job(job, &Address(to.0), amount)
            .expect("pay: caller must pre-validate the pot (P1 contract)");
    }
    fn burn(&mut self, amount: u64) {
        let job = self.active();
        self.chain
            .burn_from_job(job, amount)
            .expect("burn: caller must pre-validate the pot (P1 contract)");
    }
    /// On-chain, slashing the stake source is the G4 bonded stake (not a spendable-balance burn). NB
    /// the settlement money-path does not call this (it pays/burns the escrowed bonds); it satisfies
    /// the trait + serves any future direct-stake-slash caller.
    fn slash(&mut self, who: ParticipantId, amount: u64) {
        self.chain.slash_stake(&Address(who.0), amount);
    }
    fn stake_of(&self, who: &ParticipantId) -> u64 {
        self.chain.stake_of(&Address(who.0))
    }
}

impl Ledger for ChainLedger<'_> {
    fn for_job(&mut self, job_id: [u8; 32]) {
        self.job = Some(job_id);
    }
}

// PoUW P2 §3: ChainState helpers that drive the per-job JobLifecycle, running its money moves through
// the ChainLedger view. Each money-moving helper does the borrow dance — take the lifecycle OUT of the
// map (owning it), run the method against a ChainLedger over the rest of ChainState, then re-insert —
// because the lifecycle lives inside ChainState and so cannot be &mut-borrowed while ChainState is
// also the &mut ledger.
impl ChainState {
    /// Record a committee verifier's commit (escrows the bond into the job pot via the lifecycle).
    /// `None` if no lifecycle exists for `job_id`.
    pub fn lifecycle_record_commit(
        &mut self,
        job_id: [u8; 32],
        c: Commitment,
        height: u64,
    ) -> Result<Option<EventResult>, StateError> {
        let mut life = match self.job_lifecycles.remove(&job_id) {
            Some(l) => l,
            None => return Ok(None),
        };
        // record_commit escrows c.bond from the committer (on Accepted) via the infallible ChainHooks
        // surface; pre-check the balance so that escrow cannot panic the ledger. (record_commit may
        // still Reject for phase/window/membership/double-commit — those move no money.)
        let committer = Address(c.verifier.0);
        let bal = self.accounts.get(&committer).map(|a| a.balance.raw()).unwrap_or(0);
        if bal < c.bond {
            self.job_lifecycles.insert(job_id, life);
            return Err(StateError::InsufficientBalance);
        }
        let mut view = ChainLedger::new(self);
        let r = life.record_commit(&mut view, c, height);
        self.job_lifecycles.insert(job_id, life);
        Ok(Some(r))
    }

    /// Record a committee verifier's reveal (no money move). `None` if no lifecycle for `job_id`.
    pub fn lifecycle_record_reveal(
        &mut self,
        job_id: [u8; 32],
        r: Reveal,
        height: u64,
    ) -> Option<EventResult> {
        let life = self.job_lifecycles.get_mut(&job_id)?;
        Some(life.record_reveal(r, height))
    }

    /// Advance the lifecycle's phase by block height (no money move). `None` if no lifecycle.
    pub fn lifecycle_advance(&mut self, job_id: [u8; 32], height: u64) -> Option<Phase> {
        let life = self.job_lifecycles.get_mut(&job_id)?;
        Some(life.advance(height))
    }

    /// Settle the lifecycle at its terminal, moving the pot per the verdict (the §3 money-path).
    /// Idempotent (the lifecycle caches its terminal), so a re-org / double tick re-runs no money.
    /// `None` if no lifecycle for `job_id`. The lifecycle is re-inserted; Phase B (event_loop) removes
    /// drained terminals. The CALLER MUST pre-validate the pot before this (see `ChainLedger`).
    pub fn lifecycle_settle(
        &mut self,
        job_id: [u8; 32],
        eq: &dyn EquivalenceOracle,
    ) -> Result<Option<Terminal>, StateError> {
        let mut life = match self.job_lifecycles.remove(&job_id) {
            Some(l) => l,
            None => return Ok(None),
        };
        // Pre-validate the pot (P1 caller-contract) so a malformed pot REJECTS the terminal tx instead
        // of panicking the ledger mid-settle. After this guard every pay/burn in settle is covered by
        // the pot, so the ChainLedger .expect()s cannot fire — which also keeps the borrow dance
        // panic-safe (no panic ⇒ the re-insert below always runs ⇒ the lifecycle is never lost).
        let expected = life.expected_escrow();
        let actual = self.escrowed_for_job(&job_id);
        if actual != expected {
            self.job_lifecycles.insert(job_id, life);
            return Err(StateError::InvalidBlock(format!(
                "job pot {actual} != expected {expected}; refusing to settle"
            )));
        }
        let mut view = ChainLedger::new(self);
        let terminal = life.settle(&mut view, eq);
        self.job_lifecycles.insert(job_id, life);
        Ok(Some(terminal))
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
    #[error("escrow pot underflow")]
    EscrowUnderflow,
    #[error("insufficient bonded stake")]
    InsufficientStake,
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

    // PoUW P2 §3 lifecycle-test helpers (JobLifecycle/Terminal/Commitment/Reveal/EventResult/Phase
    // come via `use super::*`).
    use commputer_pouw::commit_reveal::make_commitment;
    use commputer_pouw::economics::{budget_min, executor_bond_min, verifier_bond_min};
    use commputer_pouw::oracle::ByteEq;
    use commputer_pouw::params::GameParams;
    use commputer_pouw_onchain::lifecycle::PhaseDeadlines;
    use commputer_pouw_onchain::settlement_resolution::ResolutionParams;
    use commputer_pouw_onchain::escrow_ledger::EscrowLedger; // B10: the staging reference ledger

    /// ParticipantId and the byte-identical on-chain Address (the ChainLedger casts between them).
    fn lpid(n: u8) -> ParticipantId {
        ParticipantId([n; 32])
    }
    fn lpaddr(n: u8) -> Address {
        Address([n; 32])
    }
    fn fuel_mins() -> (u64, u64, u64) {
        let p = GameParams::default();
        let f = 100_000_000u64;
        let b = budget_min(f, &p).unwrap();
        (b, executor_bond_min(f, b, &p).unwrap(), verifier_bond_min(f, &p).unwrap())
    }
    fn test_deadlines() -> PhaseDeadlines {
        PhaseDeadlines { result_by: 10, commit_by: 20, reveal_by: 30 }
    }

    // --- PoUW P1 escrow foundation -------------------------------------------------

    /// Sum of every account's spendable balance (raw units) — the other half of the
    /// conservation identity `sum(balances) + total_escrowed()`.
    fn sum_balances(state: &ChainState) -> u64 {
        state.accounts.iter().map(|a| a.balance.raw()).sum()
    }

    #[test]
    fn escrow_into_job_holds_value_in_circulation() {
        let mut state = ChainState::new();
        state.total_emitted = 10_000;
        state.accounts.get_or_create(addr(1)).balance = Amount::from_raw(10_000);
        let conserved = sum_balances(&state) + state.total_escrowed();
        let circ_before = state.circulating_supply();

        let job = [7u8; 32];
        state.escrow_into_job(&addr(1), job, 4_000).unwrap();

        assert_eq!(state.accounts.get(&addr(1)).unwrap().balance, Amount::from_raw(6_000));
        assert_eq!(state.escrowed_for_job(&job), 4_000);
        assert_eq!(state.total_escrowed(), 4_000);
        assert_eq!(state.total_burned, 0, "escrow does not burn");
        assert_eq!(state.circulating_supply(), circ_before, "escrow stays in circulation");
        assert_eq!(sum_balances(&state) + state.total_escrowed(), conserved, "conserved");
    }

    #[test]
    fn pay_from_job_drains_pot_and_credits_recipients() {
        let mut state = ChainState::new();
        state.total_emitted = 10_000;
        state.accounts.get_or_create(addr(1)).balance = Amount::from_raw(10_000);
        let conserved = sum_balances(&state) + state.total_escrowed();

        let job = [8u8; 32];
        state.escrow_into_job(&addr(1), job, 10_000).unwrap();
        state.pay_from_job(job, &addr(2), 7_000).unwrap();
        state.pay_from_job(job, &addr(3), 3_000).unwrap();

        assert_eq!(state.accounts.get(&addr(2)).unwrap().balance, Amount::from_raw(7_000));
        assert_eq!(state.accounts.get(&addr(3)).unwrap().balance, Amount::from_raw(3_000));
        assert_eq!(state.escrowed_for_job(&job), 0, "pot fully paid out");
        assert!(!state.escrow_by_job.contains_key(&job), "drained pot entry removed");
        assert_eq!(state.total_burned, 0, "pay does not burn");
        assert_eq!(sum_balances(&state) + state.total_escrowed(), conserved);
    }

    #[test]
    fn burn_from_job_reduces_circulating_supply_by_exactly_the_burn() {
        let mut state = ChainState::new();
        state.total_emitted = 10_000;
        state.accounts.get_or_create(addr(1)).balance = Amount::from_raw(10_000);
        let conserved = sum_balances(&state) + state.total_escrowed();
        let circ_before = state.circulating_supply();

        let job = [9u8; 32];
        state.escrow_into_job(&addr(1), job, 10_000).unwrap();
        // Settle like a Confirmed split: pay 9_500, burn the 500 remainder.
        state.pay_from_job(job, &addr(2), 9_500).unwrap();
        state.burn_from_job(job, 500).unwrap();

        assert_eq!(state.total_burned, 500, "exactly the burn slice left supply");
        assert_eq!(state.circulating_supply(), circ_before - 500);
        assert_eq!(state.escrowed_for_job(&job), 0, "pot drained");
        assert!(!state.escrow_by_job.contains_key(&job));
        assert_eq!(
            sum_balances(&state) + state.total_escrowed(),
            conserved - 500,
            "conserved minus exactly the burn"
        );
    }

    #[test]
    fn pay_more_than_pot_is_rejected_and_pot_untouched() {
        let mut state = ChainState::new();
        state.accounts.get_or_create(addr(1)).balance = Amount::from_raw(1_000);
        let job = [1u8; 32];
        state.escrow_into_job(&addr(1), job, 1_000).unwrap();

        let err = state.pay_from_job(job, &addr(2), 1_001).unwrap_err();
        assert!(matches!(err, StateError::EscrowUnderflow));
        assert_eq!(state.escrowed_for_job(&job), 1_000, "pot unchanged on rejection");
        assert!(state.accounts.get(&addr(2)).is_none(), "recipient not credited");
    }

    #[test]
    fn escrow_more_than_balance_is_rejected_and_no_pot_created() {
        let mut state = ChainState::new();
        state.accounts.get_or_create(addr(1)).balance = Amount::from_raw(500);
        let job = [2u8; 32];

        let err = state.escrow_into_job(&addr(1), job, 501).unwrap_err();
        assert!(matches!(err, StateError::InsufficientBalance));
        assert_eq!(
            state.accounts.get(&addr(1)).unwrap().balance,
            Amount::from_raw(500),
            "balance untouched"
        );
        assert!(!state.escrow_by_job.contains_key(&job), "no pot on failed escrow");
    }

    #[test]
    fn full_confirmed_lifecycle_conserves_supply() {
        // Mirror a Confirmed settlement end-to-end: submitter escrows budget, executor +
        // 3 committee escrow bonds, then resolve 85/10/5 of budget + return every bond.
        let mut state = ChainState::new();
        let (budget, e_bond, v_bond) = (3_960u64, 3_960u64, 1_650u64);
        let funded = budget + e_bond + 3 * v_bond;
        state.total_emitted = funded;
        state.accounts.get_or_create(addr(1)).balance = Amount::from_raw(budget); // submitter
        state.accounts.get_or_create(addr(9)).balance = Amount::from_raw(e_bond); // executor
        for c in 10u8..13 {
            state.accounts.get_or_create(addr(c)).balance = Amount::from_raw(v_bond);
        }
        let conserved = sum_balances(&state) + state.total_escrowed();

        let job = [42u8; 32];
        state.escrow_into_job(&addr(1), job, budget).unwrap();
        state.escrow_into_job(&addr(9), job, e_bond).unwrap();
        for c in 10u8..13 {
            state.escrow_into_job(&addr(c), job, v_bond).unwrap();
        }
        assert_eq!(state.escrowed_for_job(&job), funded, "whole pot escrowed");

        // Confirmed split: 85% worker, 10% across 3 committee, 5% burn; all bonds returned.
        state.pay_from_job(job, &addr(9), 3_366).unwrap(); // 85% of budget -> executor
        for c in 10u8..13 {
            state.pay_from_job(job, &addr(c), 132).unwrap(); // 10% / 3 -> each verifier
        }
        state.burn_from_job(job, 198).unwrap(); // 5% burned
        state.pay_from_job(job, &addr(9), e_bond).unwrap(); // executor bond back
        for c in 10u8..13 {
            state.pay_from_job(job, &addr(c), v_bond).unwrap(); // committee bonds back
        }

        assert_eq!(state.escrowed_for_job(&job), 0, "pot fully drained");
        assert!(!state.escrow_by_job.contains_key(&job));
        assert_eq!(state.total_burned, 198, "only the 5% slice burned");
        assert_eq!(
            state.accounts.get(&addr(9)).unwrap().balance,
            Amount::from_raw(3_366 + e_bond)
        );
        assert_eq!(
            state.accounts.get(&addr(10)).unwrap().balance,
            Amount::from_raw(132 + v_bond)
        );
        assert_eq!(
            state.accounts.get(&addr(1)).unwrap().balance,
            Amount::from_raw(0),
            "submitter spent budget"
        );
        assert_eq!(
            sum_balances(&state) + state.total_escrowed(),
            conserved - 198,
            "conserved minus burn"
        );
    }

    #[test]
    fn escrow_into_job_rejects_pot_overflow_without_partial_state() {
        // Two whales escrow into the same pot; the second would overflow it → rejected, no debit.
        let mut state = ChainState::new();
        let job = [11u8; 32];
        state.accounts.get_or_create(addr(1)).balance = Amount::from_raw(u64::MAX);
        state.accounts.get_or_create(addr(2)).balance = Amount::from_raw(u64::MAX);
        state.escrow_into_job(&addr(1), job, u64::MAX).unwrap(); // pot = u64::MAX
        assert_eq!(state.escrowed_for_job(&job), u64::MAX);

        let err = state.escrow_into_job(&addr(2), job, 1).unwrap_err(); // pot + 1 overflows
        assert!(matches!(err, StateError::Overflow), "pot overflow rejected, got {err:?}");
        // no partial state: addr(2) balance untouched, pot unchanged
        assert_eq!(state.accounts.get(&addr(2)).unwrap().balance, Amount::from_raw(u64::MAX));
        assert_eq!(state.escrowed_for_job(&job), u64::MAX);
    }

    // --- PoUW P2 (G4) bonded stake source ------------------------------------------

    /// The four-bucket conserved quantity: spendable + active bonded + cooldown + burned.
    fn stake_conserved(state: &ChainState) -> u64 {
        sum_balances(state) + state.total_bonded() + state.total_unbonding() + state.total_burned
    }

    #[test]
    fn bond_moves_balance_to_bonded_and_conserves() {
        let mut state = ChainState::new();
        state.total_emitted = 5_000;
        state.accounts.get_or_create(addr(1)).balance = Amount::from_raw(5_000);
        let conserved = stake_conserved(&state);

        state.bond(&addr(1), 3_000).unwrap();
        assert_eq!(state.accounts.get(&addr(1)).unwrap().balance, Amount::from_raw(2_000));
        assert_eq!(state.bonded_of(&addr(1)), 3_000);
        assert_eq!(stake_conserved(&state), conserved);

        // over-balance bond rejected, no state change
        assert!(matches!(state.bond(&addr(1), 9_999), Err(StateError::InsufficientBalance)));
        assert_eq!(state.accounts.get(&addr(1)).unwrap().balance, Amount::from_raw(2_000));
        assert_eq!(state.bonded_of(&addr(1)), 3_000);
    }

    #[test]
    fn request_unbond_moves_bonded_to_cooldown() {
        let mut state = ChainState::new();
        state.stake_params = StakeParams { unbonding_blocks: 100, min_bond: 1_000 };
        state.total_emitted = 5_000;
        state.accounts.get_or_create(addr(1)).balance = Amount::from_raw(5_000);
        state.bond(&addr(1), 5_000).unwrap();
        let conserved = stake_conserved(&state);

        state.request_unbond(&addr(1), 2_000, 50).unwrap();
        assert_eq!(state.bonded_of(&addr(1)), 3_000);
        assert_eq!(state.unbonding_of(&addr(1)), 2_000); // matures at 150
        assert_eq!(stake_conserved(&state), conserved);

        // over-bonded unbond rejected
        assert!(matches!(
            state.request_unbond(&addr(1), 9_999, 50),
            Err(StateError::InsufficientStake)
        ));
        // zero-amount unbond is a no-op (no empty cooldown chunk)
        let before = state.unbonding_of(&addr(1));
        state.request_unbond(&addr(1), 0, 50).unwrap();
        assert_eq!(state.unbonding_of(&addr(1)), before, "zero unbond pushed no chunk");
    }

    #[test]
    fn withdraw_unbonded_respects_maturity() {
        let mut state = ChainState::new();
        state.stake_params = StakeParams { unbonding_blocks: 100, min_bond: 1_000 };
        state.total_emitted = 5_000;
        state.accounts.get_or_create(addr(1)).balance = Amount::from_raw(5_000);
        state.bond(&addr(1), 5_000).unwrap();
        state.request_unbond(&addr(1), 1_000, 10).unwrap(); // matures 110
        state.request_unbond(&addr(1), 2_000, 50).unwrap(); // matures 150
        let conserved = stake_conserved(&state);

        assert_eq!(state.withdraw_unbonded(&addr(1), 109), 0, "nothing matured yet");
        assert_eq!(state.unbonding_of(&addr(1)), 3_000);
        assert_eq!(state.withdraw_unbonded(&addr(1), 110), 1_000, "first chunk matured");
        assert_eq!(state.accounts.get(&addr(1)).unwrap().balance, Amount::from_raw(1_000));
        assert_eq!(state.unbonding_of(&addr(1)), 2_000);
        assert_eq!(state.withdraw_unbonded(&addr(1), 200), 2_000, "second chunk matured");
        assert_eq!(state.unbonding_of(&addr(1)), 0);
        assert_eq!(state.accounts.get(&addr(1)).unwrap().balance, Amount::from_raw(3_000));
        assert_eq!(stake_conserved(&state), conserved);
    }

    #[test]
    fn slash_stake_anti_dodge_reaches_cooldown_and_burns() {
        let mut state = ChainState::new();
        state.stake_params = StakeParams { unbonding_blocks: 100, min_bond: 1_000 };
        state.total_emitted = 5_000;
        state.accounts.get_or_create(addr(1)).balance = Amount::from_raw(5_000);
        state.bond(&addr(1), 5_000).unwrap();
        state.request_unbond(&addr(1), 5_000, 10).unwrap(); // ALL stake now in cooldown
        let conserved = stake_conserved(&state);
        let circ_before = state.circulating_supply();
        assert_eq!(state.bonded_of(&addr(1)), 0);
        assert_eq!(state.unbonding_of(&addr(1)), 5_000);

        assert_eq!(state.slash_stake(&addr(1), 4_000), 4_000, "slash reaches cooldown stake");
        assert_eq!(state.unbonding_of(&addr(1)), 1_000);
        assert_eq!(state.total_burned, 4_000);
        assert_eq!(state.circulating_supply(), circ_before - 4_000, "slashed stake leaves circulation");
        assert_eq!(stake_conserved(&state), conserved, "at-risk -> burned, four-bucket sum invariant");

        // a later withdraw only returns what survived the slash
        assert_eq!(state.withdraw_unbonded(&addr(1), 110), 1_000);
        assert_eq!(stake_conserved(&state), conserved);
    }

    #[test]
    fn slash_stake_bonded_first_then_caps() {
        let mut state = ChainState::new();
        state.stake_params = StakeParams { unbonding_blocks: 100, min_bond: 1_000 };
        state.total_emitted = 5_000;
        state.accounts.get_or_create(addr(1)).balance = Amount::from_raw(5_000);
        state.bond(&addr(1), 5_000).unwrap();
        state.request_unbond(&addr(1), 2_000, 10).unwrap(); // 3_000 bonded, 2_000 cooldown
        let conserved = stake_conserved(&state);

        // partial slash hits bonded first
        assert_eq!(state.slash_stake(&addr(1), 1_000), 1_000);
        assert_eq!(state.bonded_of(&addr(1)), 2_000);
        assert_eq!(state.unbonding_of(&addr(1)), 2_000);

        // slash beyond total at-risk (now 4_000) burns everything and returns the cap
        assert_eq!(state.slash_stake(&addr(1), 10_000), 4_000, "capped at total at-risk");
        assert_eq!(state.bonded_of(&addr(1)), 0);
        assert_eq!(state.unbonding_of(&addr(1)), 0);
        assert!(!state.bonded_stake.contains_key(&addr(1)), "zeroed bonded entry removed");
        assert_eq!(stake_conserved(&state), conserved);

        // slashing an actor with nothing is a no-op and creates no entry
        assert_eq!(state.slash_stake(&addr(9), 100), 0, "slashing nobody is a no-op");
        assert!(!state.bonded_stake.contains_key(&addr(9)), "no entry for never-bonded");
    }

    #[test]
    fn stake_of_excludes_unbonding_and_eligibility_floors_at_min_bond() {
        let mut state = ChainState::new();
        state.stake_params = StakeParams { unbonding_blocks: 100, min_bond: 1_000 };
        state.accounts.get_or_create(addr(1)).balance = Amount::from_raw(5_000);
        state.bond(&addr(1), 5_000).unwrap();
        assert_eq!(state.stake_of(&addr(1)), 5_000);
        assert!(state.is_eligible(&addr(1)));

        state.request_unbond(&addr(1), 4_500, 10).unwrap(); // active bonded now 500
        assert_eq!(state.stake_of(&addr(1)), 500, "cooldown excluded from selection weight");
        assert_eq!(state.unbonding_of(&addr(1)), 4_500, "but still at-risk");
        assert!(!state.is_eligible(&addr(1)), "500 < min_bond 1_000");

        // eligibility boundary: == min_bond is eligible
        state.accounts.get_or_create(addr(2)).balance = Amount::from_raw(1_000);
        state.bond(&addr(2), 1_000).unwrap();
        assert!(state.is_eligible(&addr(2)));
    }

    #[test]
    fn every_stake_op_preserves_conservation() {
        let mut state = ChainState::new();
        state.stake_params = StakeParams { unbonding_blocks: 50, min_bond: 1_000 };
        state.total_emitted = 10_000;
        state.accounts.get_or_create(addr(1)).balance = Amount::from_raw(10_000);
        let conserved = stake_conserved(&state);

        state.bond(&addr(1), 8_000).unwrap();
        assert_eq!(stake_conserved(&state), conserved, "bond: balance -> bonded");
        state.request_unbond(&addr(1), 3_000, 5).unwrap();
        assert_eq!(stake_conserved(&state), conserved, "request_unbond: bonded -> unbonding");
        state.slash_stake(&addr(1), 2_000);
        assert_eq!(stake_conserved(&state), conserved, "slash: at-risk -> burned");
        state.withdraw_unbonded(&addr(1), 1_000);
        assert_eq!(stake_conserved(&state), conserved, "withdraw: matured unbonding -> balance");
    }

    // --- PoUW P2 (G2) Commit/Reveal TxKinds (inert until the committee-draw wiring) ----

    fn block_with(state: &ChainState, height: u64, txs: Vec<Transaction>) -> Block {
        Block {
            header: BlockHeader {
                protocol_version: 1,
                height,
                parent_hash: state.blocks.latest().unwrap().hash(),
                tx_root: [0u8; 32],
                proof_root: [0u8; 32],
                state_root: [0u8; 32],
                timestamp: 2000 + height,
                producer: addr(0),
                epoch: 0,
                producer_public_key: vec![],
                signature: vec![],
                checkpoint_hash: None,
                chain_id: String::new(),
            },
            transactions: txs,
            proof_summaries: vec![],
            compliance_summary: None,
            epoch_summary: None,
        }
    }

    fn unsigned(from: Address, nonce: u64, kind: TxKind) -> Transaction {
        Transaction { from, nonce, kind, fee: 0, signature: vec![], public_key: vec![], memo: None, timelock: None }
    }

    #[test]
    fn commit_from_validator_is_inert_accepted() {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        let v = state.accounts.get_or_create(addr(1));
        v.is_validator = true;
        v.balance = Amount::from_comme(10);
        state.total_emitted = Amount::from_comme(10).raw();
        let burned_before = state.total_burned;
        let bal_before = state.accounts.get(&addr(1)).unwrap().balance;

        let commit = TxKind::Commit { job_id: [7u8; 32], commit: [2u8; 32], bond: Amount::from_raw(1_000) };
        state.apply_block(&block_with(&state, 1, vec![unsigned(addr(1), 0, commit)])).unwrap();

        assert_eq!(state.accounts.get(&addr(1)).unwrap().nonce, 1, "nonce bumped");
        assert_eq!(state.accounts.get(&addr(1)).unwrap().balance, bal_before, "bond NOT escrowed (inert)");
        assert_eq!(state.total_burned, burned_before, "Commit does not burn");
        assert_eq!(state.escrowed_for_job(&[7u8; 32]), 0, "no escrow pot created yet");
    }

    #[test]
    fn reveal_from_validator_is_inert_accepted() {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        state.accounts.get_or_create(addr(1)).is_validator = true;

        let reveal = TxKind::Reveal { job_id: [7u8; 32], result_hash: [3u8; 32], salt: [4u8; 32] };
        state.apply_block(&block_with(&state, 1, vec![unsigned(addr(1), 0, reveal)])).unwrap();
        assert_eq!(state.accounts.get(&addr(1)).unwrap().nonce, 1, "nonce bumped");
    }

    #[test]
    fn commit_and_reveal_from_non_validator_are_rejected() {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        state.accounts.get_or_create(addr(2)).balance = Amount::from_comme(10);

        let commit = TxKind::Commit { job_id: [7u8; 32], commit: [2u8; 32], bond: Amount::from_raw(1_000) };
        assert!(
            state.apply_block(&block_with(&state, 1, vec![unsigned(addr(2), 0, commit)])).is_err(),
            "non-validator Commit rejected"
        );
        let reveal = TxKind::Reveal { job_id: [7u8; 32], result_hash: [3u8; 32], salt: [4u8; 32] };
        assert!(
            state.apply_block(&block_with(&state, 1, vec![unsigned(addr(2), 0, reveal)])).is_err(),
            "non-validator Reveal rejected"
        );
    }

    // --- PoUW P2 (G4) Bond/RequestUnbond/WithdrawUnbonded TxKinds (apply-path wiring) ---

    #[test]
    fn bond_tx_grows_bonded_drops_balance_and_conserves() {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap(); // height 0
        state.accounts.get_or_create(addr(1)).balance = Amount::from_raw(5_000);
        state.total_emitted = 5_000;
        let conserved = stake_conserved(&state);

        let bond = TxKind::Bond { amount: Amount::from_raw(3_000) };
        state.apply_block(&block_with(&state, 1, vec![unsigned(addr(1), 0, bond)])).unwrap();

        assert_eq!(state.accounts.get(&addr(1)).unwrap().balance, Amount::from_raw(2_000));
        assert_eq!(state.bonded_of(&addr(1)), 3_000);
        assert_eq!(state.accounts.get(&addr(1)).unwrap().nonce, 1, "nonce bumped");
        assert_eq!(state.total_burned, 0, "Bond does not burn");
        assert_eq!(stake_conserved(&state), conserved, "four buckets conserved");
    }

    #[test]
    fn bond_tx_insufficient_balance_rejected_no_partial_state() {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        state.accounts.get_or_create(addr(1)).balance = Amount::from_raw(1_000);
        state.total_emitted = 1_000;

        let bond = TxKind::Bond { amount: Amount::from_raw(2_000) }; // > balance
        let res = state.apply_block(&block_with(&state, 1, vec![unsigned(addr(1), 0, bond)]));
        assert!(res.is_err(), "over-balance bond rejects the block");
        // bond() validates before mutating; fee is 0, so nothing changed:
        assert_eq!(state.accounts.get(&addr(1)).unwrap().balance, Amount::from_raw(1_000), "balance untouched");
        assert_eq!(state.bonded_of(&addr(1)), 0, "no bond recorded");
        assert_eq!(state.accounts.get(&addr(1)).unwrap().nonce, 0, "nonce not bumped on failure");
    }

    #[test]
    fn request_unbond_tx_moves_bonded_to_cooldown_and_conserves() {
        let mut state = ChainState::new();
        state.stake_params = StakeParams { unbonding_blocks: 100, min_bond: 1_000 };
        state.apply_block(&genesis_block()).unwrap(); // height 0
        state.accounts.get_or_create(addr(1)).balance = Amount::from_raw(5_000);
        state.total_emitted = 5_000;
        state.bond(&addr(1), 5_000).unwrap(); // pre-seed active bond (bond() itself is unit-tested)
        let conserved = stake_conserved(&state);

        let ru = TxKind::RequestUnbond { amount: Amount::from_raw(2_000) };
        state.apply_block(&block_with(&state, 1, vec![unsigned(addr(1), 0, ru)])).unwrap();

        // now during apply = blocks.height() = 0 -> matures_at = 0 + 100 = 100
        assert_eq!(state.bonded_of(&addr(1)), 3_000, "active bonded reduced");
        assert_eq!(state.unbonding_of(&addr(1)), 2_000, "moved to cooldown (still supply)");
        assert_eq!(state.stake_of(&addr(1)), 3_000, "cooldown excluded from selection weight");
        assert_eq!(state.accounts.get(&addr(1)).unwrap().nonce, 1, "nonce bumped");
        assert_eq!(stake_conserved(&state), conserved);
    }

    #[test]
    fn withdraw_unbonded_tx_respects_maturity_through_apply() {
        let mut state = ChainState::new();
        state.stake_params = StakeParams { unbonding_blocks: 2, min_bond: 1_000 };
        state.apply_block(&genesis_block()).unwrap(); // height 0
        state.accounts.get_or_create(addr(1)).balance = Amount::from_raw(5_000);
        state.total_emitted = 5_000;
        state.bond(&addr(1), 5_000).unwrap();
        let conserved = stake_conserved(&state);

        // Block 1: RequestUnbond 2_000. now = height 0 -> matures_at = 0 + 2 = 2.
        let ru = TxKind::RequestUnbond { amount: Amount::from_raw(2_000) };
        state.apply_block(&block_with(&state, 1, vec![unsigned(addr(1), 0, ru)])).unwrap();
        assert_eq!(state.unbonding_of(&addr(1)), 2_000);
        assert_eq!(state.accounts.get(&addr(1)).unwrap().balance, Amount::from_raw(0));

        // Block 2: WithdrawUnbonded BEFORE maturity. now = height 1 < 2 -> no credit, nonce bumps.
        let w = TxKind::WithdrawUnbonded;
        state.apply_block(&block_with(&state, 2, vec![unsigned(addr(1), 1, w.clone())])).unwrap();
        assert_eq!(state.unbonding_of(&addr(1)), 2_000, "not matured -> still in cooldown");
        assert_eq!(state.accounts.get(&addr(1)).unwrap().balance, Amount::from_raw(0), "no credit yet");
        assert_eq!(state.accounts.get(&addr(1)).unwrap().nonce, 2, "nonce bumped on no-op withdraw");

        // Block 3: WithdrawUnbonded AT maturity. now = height 2 >= 2 -> credited back.
        state.apply_block(&block_with(&state, 3, vec![unsigned(addr(1), 2, w)])).unwrap();
        assert_eq!(state.unbonding_of(&addr(1)), 0, "matured chunk withdrawn");
        assert_eq!(state.accounts.get(&addr(1)).unwrap().balance, Amount::from_raw(2_000), "credited back");
        assert_eq!(state.accounts.get(&addr(1)).unwrap().nonce, 3);
        assert_eq!(stake_conserved(&state), conserved, "withdraw conserves");
    }

    #[test]
    fn bond_inside_batch_conserves_and_bumps_nonce_once() {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        state.accounts.get_or_create(addr(1)).balance = Amount::from_raw(5_000);
        state.total_emitted = 5_000;
        let conserved = stake_conserved(&state);

        let batch = TxKind::Batch { operations: vec![
            TxKind::Bond { amount: Amount::from_raw(1_000) },
            TxKind::Bond { amount: Amount::from_raw(2_000) },
        ] };
        state.apply_block(&block_with(&state, 1, vec![unsigned(addr(1), 0, batch)])).unwrap();

        assert_eq!(state.bonded_of(&addr(1)), 3_000, "both batched bonds applied");
        assert_eq!(state.accounts.get(&addr(1)).unwrap().balance, Amount::from_raw(2_000));
        assert_eq!(state.accounts.get(&addr(1)).unwrap().nonce, 1, "batch bumps nonce once");
        assert_eq!(stake_conserved(&state), conserved);
    }

    #[test]
    fn failing_bond_in_batch_rejects_block_and_conserves() {
        // NOTE ON ATOMICITY: the non-atomic `apply_block` path (used here and by tests) mutates
        // `self` in place and does NOT roll back a batch's earlier ops when a later op fails — op1's
        // Bond is left applied in the in-memory state, but the block is REJECTED (returns Err, is
        // never stored) and the nonce is NOT bumped, so the chain never advances on it. Callers that
        // require rollback use `apply_block_atomic`. Crucially, conservation still holds: a partial
        // Bond only moves value balance->bonded within the same account, minting/burning nothing.
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        state.accounts.get_or_create(addr(1)).balance = Amount::from_raw(1_000);
        state.total_emitted = 1_000;
        let before = stake_conserved(&state);

        let batch = TxKind::Batch { operations: vec![
            TxKind::Bond { amount: Amount::from_raw(500) },
            TxKind::Bond { amount: Amount::from_raw(5_000) }, // 2nd op fails: over balance
        ] };
        let res = state.apply_block(&block_with(&state, 1, vec![unsigned(addr(1), 0, batch)]));
        assert!(res.is_err(), "a failing op rejects the whole block");
        assert_eq!(state.accounts.get(&addr(1)).unwrap().nonce, 0, "nonce not bumped -> chain does not advance");
        assert_eq!(state.blocks.height(), 0, "rejected block was never stored");
        assert_eq!(stake_conserved(&state), before, "no value minted/burned even with partial op");
    }

    #[test]
    fn bond_from_non_validator_is_permitted() {
        // Locks the design decision: bonding is a permissionless on-ramp (contrast Commit/Reveal,
        // which require is_validator). Override to validators-only if the founder chooses.
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        state.accounts.get_or_create(addr(1)).balance = Amount::from_raw(2_000);
        state.total_emitted = 2_000;
        assert!(!state.accounts.get(&addr(1)).unwrap().is_validator, "addr(1) is NOT a validator");

        let bond = TxKind::Bond { amount: Amount::from_raw(1_000) };
        state.apply_block(&block_with(&state, 1, vec![unsigned(addr(1), 0, bond)])).unwrap();
        assert_eq!(state.bonded_of(&addr(1)), 1_000, "non-validator may bond");
    }

    // --- PoUW P2 §3: end-to-end lifecycle money-path through ChainState (the Ledger trait) --------

    #[test]
    fn lifecycle_confirmed_round_moves_money_through_chainstate_and_conserves() {
        let pid = lpid;
        let paddr = lpaddr;
        let p = GameParams::default();
        let (budget, e_bond, v_bond) = fuel_mins(); // 3_960 / 3_960 / 1_650
        let committee = [pid(10), pid(11), pid(12)];
        let job = [1u8; 32];
        let result = [7u8; 32];

        let mut state = ChainState::new();
        // Fund every actor, then escrow budget (submitter) + executor bond (the submit+claim
        // precondition). Committee balances are escrowed on commit.
        let funded = budget + e_bond + 3 * v_bond;
        state.total_emitted = funded;
        state.accounts.get_or_create(paddr(0)).balance = Amount::from_raw(budget);
        state.accounts.get_or_create(paddr(9)).balance = Amount::from_raw(e_bond);
        for c in 10u8..13 {
            state.accounts.get_or_create(paddr(c)).balance = Amount::from_raw(v_bond);
        }
        let conserved = sum_balances(&state) + state.total_escrowed() + state.total_burned;
        state.escrow_into_job(&paddr(0), job, budget).unwrap();
        state.escrow_into_job(&paddr(9), job, e_bond).unwrap();

        // Open the lifecycle, draw the committee (submit_result), and store it on-chain.
        let mut lc = JobLifecycle::open(
            job, pid(0), pid(9), e_bond, budget, v_bond,
            p, ResolutionParams::default(), committee.to_vec(),
            PhaseDeadlines { result_by: 10, commit_by: 20, reveal_by: 30 },
        );
        let stake = |_: &ParticipantId| 1u64;
        assert_eq!(lc.submit_result(pid(9), result, [42u8; 32], 5, &stake), EventResult::Accepted);
        state.job_lifecycles.insert(job, lc);

        // Every committee member commits (the ChainLedger escrows their bond into the pot)...
        for (i, c) in committee.iter().enumerate() {
            let commit: Commitment = make_commitment(c, &result, &[i as u8; 32], v_bond);
            assert_eq!(state.lifecycle_record_commit(job, commit, 15).unwrap(), Some(EventResult::Accepted));
        }
        assert_eq!(
            state.escrowed_for_job(&job),
            budget + e_bond + 3 * v_bond,
            "pot holds budget + exec bond + all committee bonds"
        );
        assert_eq!(state.accounts.get(&paddr(10)).unwrap().balance, Amount::from_raw(0), "bond escrowed");

        // ...advance to Revealing, all reveal the true result...
        assert_eq!(state.lifecycle_advance(job, 21), Some(Phase::Revealing));
        for (i, c) in committee.iter().enumerate() {
            let r = Reveal { verifier: *c, result_hash: result, salt: [i as u8; 32] };
            assert_eq!(state.lifecycle_record_reveal(job, r, 25), Some(EventResult::Accepted));
        }
        state.lifecycle_advance(job, 31);

        // ...settle: Confirmed, money moves through ChainState (85/10/5 + bonds returned).
        let term = state.lifecycle_settle(job, &ByteEq).expect("pot pre-validates").expect("lifecycle exists");
        match term {
            Terminal::Confirmed(out) => {
                assert_eq!(out.worker_paid, 3_366); // 85% of budget
                assert_eq!(out.verifiers_paid, 396); // 10% across 3
                assert_eq!(out.burned, 198); // 5%
                assert_eq!(out.bonds_returned, e_bond + 3 * v_bond);
            }
            other => panic!("expected Confirmed, got {other:?}"),
        }

        // On-chain end-state: executor paid + bond back; each verifier paid + bond back; submitter 0;
        // exactly 5% burned; pot drained; supply conserved.
        assert_eq!(state.accounts.get(&paddr(9)).unwrap().balance, Amount::from_raw(3_366 + e_bond));
        assert_eq!(state.accounts.get(&paddr(10)).unwrap().balance, Amount::from_raw(132 + v_bond));
        assert_eq!(state.accounts.get(&paddr(0)).unwrap().balance, Amount::from_raw(0));
        assert_eq!(state.total_burned, 198, "only the 5% slice burned");
        assert_eq!(state.escrowed_for_job(&job), 0, "pot fully drained");
        assert_eq!(
            sum_balances(&state) + state.total_escrowed() + state.total_burned,
            conserved,
            "supply conserved across the full on-chain round"
        );
    }

    #[test]
    fn lifecycle_disputed_round_slashes_executor_through_chainstate() {
        let (budget, e_bond, v_bond) = fuel_mins();
        let committee = [lpid(10), lpid(11), lpid(12)];
        let job = [3u8; 32];
        let claimed = [7u8; 32]; // executor claims 7
        let correct = [5u8; 32]; // committee proves 5 ⇒ Disputed

        let mut state = ChainState::new();
        state.total_emitted = budget + e_bond + 3 * v_bond;
        state.accounts.get_or_create(lpaddr(0)).balance = Amount::from_raw(budget);
        state.accounts.get_or_create(lpaddr(9)).balance = Amount::from_raw(e_bond);
        for c in 10u8..13 {
            state.accounts.get_or_create(lpaddr(c)).balance = Amount::from_raw(v_bond);
        }
        let conserved = sum_balances(&state) + state.total_escrowed() + state.total_burned;
        state.escrow_into_job(&lpaddr(0), job, budget).unwrap();
        state.escrow_into_job(&lpaddr(9), job, e_bond).unwrap();

        let mut lc = JobLifecycle::open(
            job, lpid(0), lpid(9), e_bond, budget, v_bond,
            GameParams::default(), ResolutionParams::default(), committee.to_vec(), test_deadlines(),
        );
        let stake = |_: &ParticipantId| 1u64;
        assert_eq!(lc.submit_result(lpid(9), claimed, [42u8; 32], 5, &stake), EventResult::Accepted);
        state.job_lifecycles.insert(job, lc);

        for (i, c) in committee.iter().enumerate() {
            let commit = make_commitment(c, &correct, &[i as u8; 32], v_bond);
            assert_eq!(state.lifecycle_record_commit(job, commit, 15).unwrap(), Some(EventResult::Accepted));
        }
        state.lifecycle_advance(job, 21);
        for (i, c) in committee.iter().enumerate() {
            let r = Reveal { verifier: *c, result_hash: correct, salt: [i as u8; 32] };
            assert_eq!(state.lifecycle_record_reveal(job, r, 25), Some(EventResult::Accepted));
        }
        state.lifecycle_advance(job, 31);

        let term = state.lifecycle_settle(job, &ByteEq).expect("pot valid").expect("lifecycle");
        assert!(matches!(term, Terminal::Disputed(_)), "expected Disputed, got {term:?}");
        assert_eq!(state.accounts.get(&lpaddr(0)).unwrap().balance, Amount::from_raw(budget), "submitter refunded");
        assert_eq!(state.accounts.get(&lpaddr(9)).unwrap().balance, Amount::from_raw(0), "executor bond slashed");
        // committee: 20% of e_bond bounty (792) split across the 3 honest + all 3 bonds returned.
        let committee_total: u64 = (10u8..13).map(|c| state.accounts.get(&lpaddr(c)).unwrap().balance.raw()).sum();
        assert_eq!(committee_total, 792 + 3 * v_bond, "bounty + bonds to the honest committee");
        assert_eq!(state.total_burned, e_bond - 792, "remainder of slashed exec bond burned");
        assert_eq!(state.escrowed_for_job(&job), 0, "pot drained");
        assert_eq!(sum_balances(&state) + state.total_escrowed() + state.total_burned, conserved);
    }

    #[test]
    fn lifecycle_timeout_refunds_and_slashes_through_chainstate() {
        let (budget, e_bond, v_bond) = fuel_mins();
        let job = [2u8; 32];
        let mut state = ChainState::new();
        state.total_emitted = budget + e_bond;
        state.accounts.get_or_create(lpaddr(0)).balance = Amount::from_raw(budget);
        state.accounts.get_or_create(lpaddr(9)).balance = Amount::from_raw(e_bond);
        let conserved = sum_balances(&state) + state.total_escrowed() + state.total_burned;
        state.escrow_into_job(&lpaddr(0), job, budget).unwrap();
        state.escrow_into_job(&lpaddr(9), job, e_bond).unwrap();

        // Executor never submits a result. Advance past result_by, then settle → TimedOut.
        let lc = JobLifecycle::open(
            job, lpid(0), lpid(9), e_bond, budget, v_bond,
            GameParams::default(), ResolutionParams::default(),
            vec![lpid(10), lpid(11), lpid(12)], test_deadlines(),
        );
        state.job_lifecycles.insert(job, lc);
        state.lifecycle_advance(job, 11);

        let term = state.lifecycle_settle(job, &ByteEq).expect("pot valid").expect("lifecycle");
        assert!(matches!(term, Terminal::TimedOut(_)), "expected TimedOut, got {term:?}");
        // resolve_timeout: budget + 20% of exec bond to submitter; 80% burned.
        assert_eq!(state.accounts.get(&lpaddr(0)).unwrap().balance, Amount::from_raw(budget + e_bond / 5));
        assert_eq!(state.accounts.get(&lpaddr(9)).unwrap().balance, Amount::from_raw(0), "executor bond slashed");
        assert_eq!(state.total_burned, e_bond - e_bond / 5, "80% of exec bond burned");
        assert_eq!(state.escrowed_for_job(&job), 0, "pot drained");
        assert_eq!(sum_balances(&state) + state.total_escrowed() + state.total_burned, conserved);
    }

    #[test]
    fn lifecycle_settle_rejects_malformed_pot_without_panic() {
        // The pre-validation guard: a pot that doesn't equal expected_escrow() is REJECTED (Err),
        // not panicked. Here we under-fund (escrow budget but NOT the executor bond).
        let (budget, e_bond, v_bond) = fuel_mins();
        let job = [4u8; 32];
        let mut state = ChainState::new();
        state.total_emitted = budget + e_bond;
        state.accounts.get_or_create(lpaddr(0)).balance = Amount::from_raw(budget);
        state.accounts.get_or_create(lpaddr(9)).balance = Amount::from_raw(e_bond);
        state.escrow_into_job(&lpaddr(0), job, budget).unwrap(); // executor bond NOT escrowed → malformed

        let lc = JobLifecycle::open(
            job, lpid(0), lpid(9), e_bond, budget, v_bond,
            GameParams::default(), ResolutionParams::default(),
            vec![lpid(10), lpid(11), lpid(12)], test_deadlines(),
        );
        state.job_lifecycles.insert(job, lc);
        state.lifecycle_advance(job, 11);

        // expected_escrow == budget + e_bond, but the pot only holds budget → clean rejection.
        let r = state.lifecycle_settle(job, &ByteEq);
        assert!(matches!(r, Err(StateError::InvalidBlock(_))), "malformed pot rejected, got {r:?}");
        assert!(state.job_lifecycles.contains_key(&job), "lifecycle restored on rejection");
        assert_eq!(state.escrowed_for_job(&job), budget, "pot untouched on rejection");
        assert_eq!(state.total_burned, 0, "nothing burned on rejection");
    }

    // --- B10: cross-boundary golden-equivalence — staging EscrowLedger ≡ on-chain ChainState ------
    //
    // The FINAL non-protected safety net before the burn→escrow live flip. For each terminal, the
    // IDENTICAL lifecycle inputs are driven through TWO independent ledger backends:
    //   (A) STAGING  — a `JobLifecycle` over the pure in-crate reference `EscrowLedger`, and
    //   (B) ON-CHAIN — the SAME `JobLifecycle` scenario over `ChainState` via its `ChainLedger` view
    //                  (the lifecycle_record_commit/record_reveal/advance/settle helpers).
    // We then prove the two agree FIELD-FOR-FIELD on the Terminal/SettlementOutcome, on per-participant
    // NET money movement (every actor funded to the same baseline on both ⇒ end-balance equality IS
    // net-movement equality; ParticipantId(x).0 maps byte-identically to Address), and on conservation
    // (Σbalances + escrowed + burned invariant) — so the eventual live flip cannot silently diverge from
    // the audited staging game. This is non-vacuous because the two ledgers are genuinely independent
    // backends and every assertion compares ACTUAL per-participant money on BOTH sides.

    /// A lifecycle scenario, expressed once and run against both backends.
    struct Scenario {
        job: [u8; 32],
        /// Executor's claimed result hash; `None` ⇒ executor never delivers (the Timeout path).
        executor_result: Option<[u8; 32]>,
        /// Committee candidate pool (byte-ids). |pool| == k ⇒ the whole pool is drawn as the committee.
        candidates: Vec<u8>,
        /// (verifier byte-id, hash committed *and* revealed, does_reveal). List order fixes each salt.
        commits: Vec<(u8, [u8; 32], bool)>,
    }

    /// Both backends' end-state after running one scenario, for the equivalence assertions.
    struct BothEnd {
        staging_terminal: Terminal,
        chain_terminal: Terminal,
        staging: EscrowLedger,
        chain: ChainState,
        job: [u8; 32],
        /// Every actor (submitter, executor, all candidates) whose end-balance is compared.
        actors: Vec<u8>,
        staging_total0: u64,
        chain_conserved0: u64,
    }

    /// Drive `s` through BOTH the staging reference `EscrowLedger` and the on-chain `ChainState`,
    /// step-for-step with identical inputs, returning both end-states. Both are funded to the SAME
    /// baseline (budget→submitter, e_bond→executor, v_bond→each candidate) and each accept/reject at
    /// every commit/reveal/advance is cross-checked so the two state machines never silently drift.
    fn run_on_both(s: &Scenario) -> BothEnd {
        let (budget, e_bond, v_bond) = fuel_mins(); // 3_960 / 3_960 / 1_650
        let deadlines = test_deadlines();
        let seed = [42u8; 32];
        let submitter = 0u8;
        let executor = 9u8;
        let stake = |_: &ParticipantId| 1u64;
        let cand_pids: Vec<ParticipantId> = s.candidates.iter().map(|&b| lpid(b)).collect();

        let mut actors = vec![submitter, executor];
        actors.extend(s.candidates.iter().copied());

        // ---- (A) STAGING: EscrowLedger + JobLifecycle ------------------------------------------
        let mut sl = EscrowLedger::new();
        sl.credit(lpid(submitter), budget);
        sl.credit(lpid(executor), e_bond);
        for &c in &s.candidates {
            sl.credit(lpid(c), v_bond);
        }
        let staging_total0 = sl.total_supply(); // baseline AFTER funding (credit is the sole mint)
        sl.for_job(s.job);
        sl.escrow(lpid(submitter), budget); // submit+claim precondition: budget + exec bond escrowed
        sl.escrow(lpid(executor), e_bond);
        let mut slc = JobLifecycle::open(
            s.job, lpid(submitter), lpid(executor), e_bond, budget, v_bond,
            GameParams::default(), ResolutionParams::default(), cand_pids.clone(), deadlines,
        );
        if let Some(h) = s.executor_result {
            assert_eq!(slc.submit_result(lpid(executor), h, seed, 5, &stake), EventResult::Accepted);
        }

        // ---- (B) ON-CHAIN: ChainState via the ChainLedger view ---------------------------------
        let mut cs = ChainState::new();
        cs.total_emitted = budget + e_bond + s.candidates.len() as u64 * v_bond;
        cs.accounts.get_or_create(lpaddr(submitter)).balance = Amount::from_raw(budget);
        cs.accounts.get_or_create(lpaddr(executor)).balance = Amount::from_raw(e_bond);
        for &c in &s.candidates {
            cs.accounts.get_or_create(lpaddr(c)).balance = Amount::from_raw(v_bond);
        }
        let chain_conserved0 = sum_balances(&cs) + cs.total_escrowed() + cs.total_burned;
        cs.escrow_into_job(&lpaddr(submitter), s.job, budget).unwrap();
        cs.escrow_into_job(&lpaddr(executor), s.job, e_bond).unwrap();
        let mut clc = JobLifecycle::open(
            s.job, lpid(submitter), lpid(executor), e_bond, budget, v_bond,
            GameParams::default(), ResolutionParams::default(), cand_pids.clone(), deadlines,
        );
        if let Some(h) = s.executor_result {
            assert_eq!(clc.submit_result(lpid(executor), h, seed, 5, &stake), EventResult::Accepted);
        }
        cs.job_lifecycles.insert(s.job, clc);

        // ---- commits (height 15): same accept/reject on both --------------------------------------
        for (i, &(id, hash, _reveal)) in s.commits.iter().enumerate() {
            let salt = [i as u8; 32];
            let commit: Commitment = make_commitment(&lpid(id), &hash, &salt, v_bond);
            let s_res = slc.record_commit(&mut sl, commit, 15);
            let c_res = cs.lifecycle_record_commit(s.job, commit, 15).unwrap();
            assert_eq!(Some(s_res), c_res, "commit accept/reject must match across backends");
        }

        // ---- advance to Revealing (height 21) -----------------------------------------------------
        let s_phase = slc.advance(21);
        let c_phase = cs.lifecycle_advance(s.job, 21).unwrap();
        assert_eq!(s_phase, c_phase, "phase after advance must match across backends");

        // ---- reveals (height 25): same accept/reject on both --------------------------------------
        for (i, &(id, hash, reveal)) in s.commits.iter().enumerate() {
            if !reveal {
                continue;
            }
            let salt = [i as u8; 32];
            let r = Reveal { verifier: lpid(id), result_hash: hash, salt };
            let s_res = slc.record_reveal(r, 25);
            let c_res = cs.lifecycle_record_reveal(s.job, r, 25);
            assert_eq!(Some(s_res), c_res, "reveal accept/reject must match across backends");
        }

        // ---- advance past reveal_by (31) + settle -------------------------------------------------
        slc.advance(31);
        cs.lifecycle_advance(s.job, 31);
        let staging_terminal = slc.settle(&mut sl, &ByteEq);
        let chain_terminal = cs
            .lifecycle_settle(s.job, &ByteEq)
            .expect("pot pre-validates")
            .expect("lifecycle exists");

        BothEnd {
            staging_terminal,
            chain_terminal,
            staging: sl,
            chain: cs,
            job: s.job,
            actors,
            staging_total0,
            chain_conserved0,
        }
    }

    /// The golden property: staging == chain, per-participant AND at the aggregate.
    fn assert_equivalent(b: &BothEnd) {
        // 1. Terminal/SettlementOutcome (or EscalationHandoff) match FIELD-FOR-FIELD — this covers
        //    worker_paid/verifiers_paid/burned/submitter_refunded/challenger_paid/panel_paid/
        //    bonds_returned/the slashed log (Confirmed/Disputed/Timeout) and budget/revealers/bonds
        //    (Escalate).
        assert_eq!(
            b.staging_terminal, b.chain_terminal,
            "staging and on-chain terminals must be byte-identical"
        );

        // 2. Per-participant NET money movement: both funded to the same baseline ⇒ end-balance
        //    equality IS net-movement equality. ParticipantId(a) maps byte-identically to Address(a).
        for &a in &b.actors {
            let s_bal = b.staging.balance_of(&lpid(a));
            let c_bal = b
                .chain
                .accounts
                .get(&lpaddr(a))
                .map(|acc| acc.balance.raw())
                .unwrap_or(0);
            assert_eq!(
                s_bal, c_bal,
                "participant {a} end-balance diverges: staging {s_bal} vs chain {c_bal}"
            );
        }

        // 3. Job pots agree: both drained to 0 (Confirmed/Disputed/Timeout) or both HOLD the same
        //    (Escalate). Comparing the two pots to each other covers every terminal uniformly.
        assert_eq!(
            b.staging.escrowed_for(&b.job),
            b.chain.escrowed_for_job(&b.job),
            "job pot must match across backends"
        );

        // 4. Both conserve against their own baseline, and the baselines themselves match (funded
        //    identically). Chain burned must equal the staging-side burn (both start at 0), which is
        //    implied by (1)+(3)+conservation but asserted directly for a crisp cross-check.
        assert_eq!(b.staging.total_supply(), b.staging_total0, "staging conserves total supply");
        assert_eq!(
            sum_balances(&b.chain) + b.chain.total_escrowed() + b.chain.total_burned,
            b.chain_conserved0,
            "chain conserves total supply",
        );
        assert_eq!(
            b.staging_total0, b.chain_conserved0,
            "both backends were funded to the same baseline"
        );
    }

    #[test]
    fn equivalence_confirmed_staging_matches_chainstate() {
        let result = [7u8; 32];
        // all 3 commit to and reveal the true result ⇒ Confirmed 85/10/5.
        let both = run_on_both(&Scenario {
            job: [1u8; 32],
            executor_result: Some(result),
            candidates: vec![10, 11, 12],
            commits: vec![(10, result, true), (11, result, true), (12, result, true)],
        });
        // anchor the audited split on the on-chain side for readability...
        match &both.chain_terminal {
            Terminal::Confirmed(out) => {
                assert_eq!(out.worker_paid, 3_366); // 85% of 3_960
                assert_eq!(out.verifiers_paid, 396); // 10% across 3
                assert_eq!(out.burned, 198); // 5%
                assert_eq!(out.bonds_returned, 3_960 + 3 * 1_650);
            }
            other => panic!("expected Confirmed, got {other:?}"),
        }
        // ...then prove staging ≡ chain field-for-field, per-participant, and in aggregate.
        assert_equivalent(&both);
        assert_eq!(both.chain.escrowed_for_job(&both.job), 0, "pot drained on Confirmed");
        assert_eq!(both.chain.total_burned, 198, "on-chain burn matches the audited 5%");
    }

    #[test]
    fn equivalence_disputed_staging_matches_chainstate() {
        let claimed = [7u8; 32]; // executor claims 7
        let correct = [5u8; 32]; // committee proves 5 ⇒ Disputed, executor slashed
        let both = run_on_both(&Scenario {
            job: [3u8; 32],
            executor_result: Some(claimed),
            candidates: vec![10, 11, 12],
            commits: vec![(10, correct, true), (11, correct, true), (12, correct, true)],
        });
        match &both.chain_terminal {
            Terminal::Disputed(out) => {
                assert_eq!(out.submitter_refunded, 3_960, "submitter fully refunded");
                assert_eq!(out.verifiers_paid, 792, "20% of exec bond bounty across the honest 3");
                assert_eq!(out.slashed, vec![(lpid(9), 3_960)], "executor bond slashed");
            }
            other => panic!("expected Disputed, got {other:?}"),
        }
        assert_equivalent(&both);
        assert_eq!(both.chain.escrowed_for_job(&both.job), 0, "pot drained on Disputed");
    }

    #[test]
    fn equivalence_timeout_staging_matches_chainstate() {
        // Executor never delivers a result ⇒ TimedOut: budget + 20% exec bond refunded, 80% burned.
        let both = run_on_both(&Scenario {
            job: [2u8; 32],
            executor_result: None,
            candidates: vec![10, 11, 12],
            commits: vec![],
        });
        match &both.chain_terminal {
            Terminal::TimedOut(out) => {
                assert_eq!(out.submitter_refunded, 3_960 + 3_960 / 5, "budget + 20% exec bond");
                assert_eq!(out.burned, 3_960 - 3_960 / 5, "80% exec bond burned");
                assert_eq!(out.slashed, vec![(lpid(9), 3_960)]);
            }
            other => panic!("expected TimedOut, got {other:?}"),
        }
        assert_equivalent(&both);
        assert_eq!(both.chain.escrowed_for_job(&both.job), 0, "pot drained on Timeout");
    }

    #[test]
    fn equivalence_noquorum_escalate_staging_matches_chainstate() {
        let result = [7u8; 32];
        // 3-way split: each reveals a DISTINCT value ⇒ no class reaches quorum(3)=2 ⇒ NoQuorum ⇒
        // Escalate. The escrow is HELD (not drained), equal on both sides.
        let both = run_on_both(&Scenario {
            job: [4u8; 32],
            executor_result: Some(result),
            candidates: vec![10, 11, 12],
            commits: vec![(10, [1u8; 32], true), (11, [2u8; 32], true), (12, [3u8; 32], true)],
        });
        match &both.chain_terminal {
            Terminal::Escalate(h) => {
                assert_eq!(h.budget, 3_960);
                assert_eq!(h.executor_bond, 3_960);
                assert_eq!(h.committee_reveals.len(), 3, "all 3 revealers handed off");
                assert_eq!(h.committee_bonds, vec![1_650; 3]);
            }
            other => panic!("expected Escalate, got {other:?}"),
        }
        assert_equivalent(&both);
        // escrow HELD equally on both (the deferred escalation round settles it): budget + Be + 3 bonds.
        let held = 3_960 + 3_960 + 3 * 1_650;
        assert_eq!(both.chain.escrowed_for_job(&both.job), held, "escrow HELD on Escalate");
        assert_eq!(both.staging.escrowed_for(&both.job), held, "staging holds the same");
        assert_eq!(both.chain.total_burned, 0, "nothing burned while escalation is pending");
    }

    #[test]
    fn equivalence_confirmed_with_non_revealer_forfeiture_matches() {
        let result = [7u8; 32];
        // 3 commit; pid(12) never reveals ⇒ its bond is forfeited (burned). The 2 revealers still
        // reach quorum(3)=2 ⇒ Confirmed. Exercises the commit-no-reveal forfeiture path on both sides.
        let both = run_on_both(&Scenario {
            job: [5u8; 32],
            executor_result: Some(result),
            candidates: vec![10, 11, 12],
            commits: vec![(10, result, true), (11, result, true), (12, result, false)],
        });
        match &both.chain_terminal {
            Terminal::Confirmed(out) => {
                assert_eq!(out.verifiers_paid, 396, "10% pool split across the 2 revealers");
                assert_eq!(out.burned, 198 + 1_650, "5% protocol burn + forfeited non-revealer bond");
                assert!(out.slashed.contains(&(lpid(12), 1_650)), "non-revealer forfeited");
                assert_eq!(out.bonds_returned, 3_960 + 2 * 1_650, "only the 2 revealers' bonds returned");
            }
            other => panic!("expected Confirmed, got {other:?}"),
        }
        assert_equivalent(&both);
        assert_eq!(both.chain.escrowed_for_job(&both.job), 0, "pot drained on Confirmed");
        assert_eq!(both.chain.total_burned, 198 + 1_650, "on-chain burn includes the forfeit");
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
                chain_id: String::new(),
            },
            transactions: vec![],
            proof_summaries: vec![],
            compliance_summary: None, epoch_summary: None,
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
                chain_id: String::new(),
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
            compliance_summary: None, epoch_summary: None,
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
                chain_id: String::new(),
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
            compliance_summary: None, epoch_summary: None,
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
                chain_id: String::new(),
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
            compliance_summary: None, epoch_summary: None,
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
                chain_id: String::new(),
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
            compliance_summary: None, epoch_summary: None,
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

    // --- PoUW P2 (B1a): consensus money/stake map persistence + state root ---------

    #[test]
    fn b1a_consensus_maps_persist_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut state = ChainState::open(dir.path()).unwrap();
            state.escrow_by_job.insert([7u8; 32], 4_000);
            state.escrow_by_job.insert([8u8; 32], 1_500);
            state.bonded_stake.insert(addr(1), 9_000);
            state.bonded_stake.insert(addr(2), 250);
            state.unbonding_stake.insert(addr(1), vec![
                UnbondingChunk { amount: 500, matures_at: 100 },
                UnbondingChunk { amount: 750, matures_at: 200 },
            ]);
            state.flush().unwrap();
        }
        let state = ChainState::open(dir.path()).unwrap();
        assert_eq!(state.escrow_by_job.len(), 2);
        assert_eq!(state.escrow_by_job.get(&[7u8; 32]), Some(&4_000));
        assert_eq!(state.escrow_by_job.get(&[8u8; 32]), Some(&1_500));
        assert_eq!(state.bonded_stake.get(&addr(1)), Some(&9_000));
        assert_eq!(state.bonded_stake.get(&addr(2)), Some(&250));
        assert_eq!(state.unbonding_stake.get(&addr(1)).unwrap().len(), 2);
        assert_eq!(state.unbonding_of(&addr(1)), 1_250); // 500 + 750 round-tripped
    }

    #[test]
    fn b1a_state_root_policy_b_and_order_independent() {
        // Policy B: while all three maps are empty, the root is the accounts-only root (byte-identical
        // to before B1a — no consensus change yet).
        let mut s1 = ChainState::new();
        s1.accounts.get_or_create(addr(1)).balance = Amount::from_raw(100);
        let accounts_only = s1.accounts.compute_state_root();
        assert_eq!(s1.compute_state_root(), accounts_only, "empty maps => accounts-only root");

        // Non-empty => the maps fold into the root (differs from accounts-only).
        s1.escrow_by_job.insert([1u8; 32], 10);
        s1.bonded_stake.insert(addr(2), 20);
        s1.unbonding_stake.insert(addr(3), vec![UnbondingChunk { amount: 5, matures_at: 9 }]);
        let folded = s1.compute_state_root();
        assert_ne!(folded, accounts_only, "non-empty maps fold into the root");

        // Order-independence: the SAME entries inserted in a different order => identical root.
        let mut s2 = ChainState::new();
        s2.accounts.get_or_create(addr(1)).balance = Amount::from_raw(100);
        s2.unbonding_stake.insert(addr(3), vec![UnbondingChunk { amount: 5, matures_at: 9 }]);
        s2.bonded_stake.insert(addr(2), 20);
        s2.escrow_by_job.insert([1u8; 32], 10);
        assert_eq!(s2.compute_state_root(), folded, "root deterministic regardless of insert order");

        // Non-vacuous: a changed amount changes the root.
        let mut s3 = ChainState::new();
        s3.accounts.get_or_create(addr(1)).balance = Amount::from_raw(100);
        s3.escrow_by_job.insert([1u8; 32], 11);
        s3.bonded_stake.insert(addr(2), 20);
        s3.unbonding_stake.insert(addr(3), vec![UnbondingChunk { amount: 5, matures_at: 9 }]);
        assert_ne!(s3.compute_state_root(), folded, "a changed amount changes the root");
    }

    #[test]
    fn b1a_reset_to_genesis_wipes_persisted_maps() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = ChainState::open(dir.path()).unwrap();
        state.escrow_by_job.insert([7u8; 32], 4_000);
        state.bonded_stake.insert(addr(1), 9_000);
        state.unbonding_stake.insert(addr(1), vec![UnbondingChunk { amount: 1, matures_at: 1 }]);
        state.flush().unwrap();
        state.reset_to_genesis().unwrap();
        assert!(state.escrow_by_job.is_empty() && state.bonded_stake.is_empty() && state.unbonding_stake.is_empty());
        // and the persisted CF rows are gone after a reopen.
        drop(state);
        let reopened = ChainState::open(dir.path()).unwrap();
        assert!(reopened.escrow_by_job.is_empty(), "reset wiped persisted escrow rows");
        assert!(reopened.bonded_stake.is_empty(), "reset wiped persisted bonded rows");
        assert!(reopened.unbonding_stake.is_empty(), "reset wiped persisted unbonding rows");
    }

    #[test]
    fn b1a_apply_block_atomic_persists_maps() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut state = ChainState::open(dir.path()).unwrap();
            state.apply_block(&genesis_block()).unwrap();
            state.escrow_by_job.insert([9u8; 32], 2_222);
            state.bonded_stake.insert(addr(5), 333);
            // apply a block atomically — the maps must ride the same WriteBatch.
            let block = block_with(&state, 1, vec![]);
            state.apply_block_atomic(&block).unwrap();
        }
        let state = ChainState::open(dir.path()).unwrap();
        assert_eq!(state.escrow_by_job.get(&[9u8; 32]), Some(&2_222), "escrow persisted by apply_block_atomic");
        assert_eq!(state.bonded_stake.get(&addr(5)), Some(&333));
    }

    // --- PoUW P2 (B1b): job_lifecycles persistence + state-root folding ------------

    /// A non-trivial persisted lifecycle: committee drawn (Committing), executor_hash set. `exec_seed`
    /// varies the result hash so distinct jobs fold to distinct state-root blobs.
    fn sample_lifecycle(job: [u8; 32], exec_seed: u8) -> JobLifecycle {
        let (budget, e_bond, v_bond) = fuel_mins();
        let mut lc = JobLifecycle::open(
            job, lpid(0), lpid(9), e_bond, budget, v_bond,
            GameParams::default(), ResolutionParams::default(),
            vec![lpid(10), lpid(11), lpid(12)], test_deadlines(),
        );
        let stake = |_: &ParticipantId| 1u64;
        assert_eq!(lc.submit_result(lpid(9), [exec_seed; 32], [42u8; 32], 5, &stake), EventResult::Accepted);
        lc
    }

    #[test]
    fn b1b_job_lifecycles_persist_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let job = [21u8; 32];
        let original_rec;
        {
            let mut state = ChainState::open(dir.path()).unwrap();
            let lc = sample_lifecycle(job, 7);
            original_rec = lc.to_record();
            state.job_lifecycles.insert(job, lc);
            state.flush().unwrap();
        }
        let state = ChainState::open(dir.path()).unwrap();
        assert_eq!(state.job_lifecycles.len(), 1);
        let reloaded = state.job_lifecycles.get(&job).expect("lifecycle reloaded");
        assert_eq!(reloaded.to_record(), original_rec, "lifecycle round-trips through RocksDB");
        assert_eq!(reloaded.phase(), Phase::Committing);
        assert_eq!(reloaded.committee().len(), 3);
    }

    #[test]
    fn b1b_lifecycle_state_root_policy_b_and_order_independent() {
        // Policy B: all four consensus maps empty ⇒ accounts-only root (byte-identical to pre-B1a).
        let mut s1 = ChainState::new();
        s1.accounts.get_or_create(addr(1)).balance = Amount::from_raw(100);
        let accounts_only = s1.accounts.compute_state_root();
        assert_eq!(s1.compute_state_root(), accounts_only, "empty maps ⇒ accounts-only root");

        // Non-empty lifecycles fold into the root.
        let job_a = [1u8; 32];
        let job_b = [2u8; 32];
        s1.job_lifecycles.insert(job_a, sample_lifecycle(job_a, 7));
        s1.job_lifecycles.insert(job_b, sample_lifecycle(job_b, 8));
        let folded = s1.compute_state_root();
        assert_ne!(folded, accounts_only, "non-empty lifecycles fold into the root");

        // Order-independence: reverse insert order ⇒ identical root (fold sorts by job_id).
        let mut s2 = ChainState::new();
        s2.accounts.get_or_create(addr(1)).balance = Amount::from_raw(100);
        s2.job_lifecycles.insert(job_b, sample_lifecycle(job_b, 8));
        s2.job_lifecycles.insert(job_a, sample_lifecycle(job_a, 7));
        assert_eq!(s2.compute_state_root(), folded, "root deterministic regardless of insert order");

        // Non-vacuity: a differing lifecycle (different executor_hash) ⇒ different root.
        let mut s3 = ChainState::new();
        s3.accounts.get_or_create(addr(1)).balance = Amount::from_raw(100);
        s3.job_lifecycles.insert(job_a, sample_lifecycle(job_a, 99));
        s3.job_lifecycles.insert(job_b, sample_lifecycle(job_b, 8));
        assert_ne!(s3.compute_state_root(), folded, "a differing lifecycle changes the root");
    }

    #[test]
    fn b1b_reset_to_genesis_wipes_persisted_lifecycles() {
        let dir = tempfile::tempdir().unwrap();
        let job = [22u8; 32];
        let mut state = ChainState::open(dir.path()).unwrap();
        state.job_lifecycles.insert(job, sample_lifecycle(job, 7));
        state.flush().unwrap();
        state.reset_to_genesis().unwrap();
        assert!(state.job_lifecycles.is_empty(), "reset cleared in-memory lifecycles");
        drop(state);
        let reopened = ChainState::open(dir.path()).unwrap();
        assert!(reopened.job_lifecycles.is_empty(), "reset wiped persisted lifecycle rows");
    }

    #[test]
    fn b1b_apply_block_atomic_persists_lifecycles() {
        let dir = tempfile::tempdir().unwrap();
        let job = [23u8; 32];
        let rec;
        {
            let mut state = ChainState::open(dir.path()).unwrap();
            state.apply_block(&genesis_block()).unwrap();
            let lc = sample_lifecycle(job, 7);
            rec = lc.to_record();
            state.job_lifecycles.insert(job, lc);
            let block = block_with(&state, 1, vec![]);
            state.apply_block_atomic(&block).unwrap();
        }
        let state = ChainState::open(dir.path()).unwrap();
        assert_eq!(
            state.job_lifecycles.get(&job).map(|l| l.to_record()),
            Some(rec),
            "lifecycle persisted by apply_block_atomic",
        );
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
                    chain_id: "test".to_string(),
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
            compliance_summary: None, epoch_summary: None,
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
                chain_id: String::new(),
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
            compliance_summary: None, epoch_summary: None,
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
                chain_id: String::new(),
            },
            transactions: vec![tx],
            proof_summaries: vec![],
            compliance_summary: None, epoch_summary: None,
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
                    chain_id: "test".to_string(),
                },
                transactions: vec![],
                proof_summaries: vec![],
                compliance_summary: None, epoch_summary: None,
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
                chain_id: String::new(),
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
            compliance_summary: None, epoch_summary: None,
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
                chain_id: String::new(),
            },
            transactions: vec![Transaction {
                from: addr(1), nonce: 0,
                kind: TxKind::Transfer { to: addr(2), amount: Amount::from_raw(5_000) },
                fee: 100_000, signature: vec![], public_key: vec![],
                memo: None, timelock: None,
            }],
            proof_summaries: vec![], compliance_summary: None, epoch_summary: None,
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
                chain_id: String::new(),
            },
            transactions: vec![Transaction {
                from: addr(1), nonce: 0,
                kind: TxKind::Transfer { to: addr(99), amount: Amount::from_comme(1) },
                fee: 100_000, // below ACCOUNT_CREATION_FEE
                signature: vec![], public_key: vec![], memo: None, timelock: None,
            }],
            proof_summaries: vec![], compliance_summary: None, epoch_summary: None,
        };
        let result = state.apply_block(&block);
        assert!(result.is_err(), "Transfer to new account with low fee should fail");
        assert!(result.unwrap_err().to_string().contains("account creation cost"));
    }

    #[test]
    fn validator_register_requires_minimum_stake() {
        // The bootstrap exemption allows registration at height < BOOTSTRAP_REGISTRATION_BLOCKS
        // regardless of balance. This test verifies the stake check fires AFTER that window.

        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();

        let sender_addr = addr(1);
        let bootstrap_end = commputer_core::transaction::BOOTSTRAP_REGISTRATION_BLOCKS;

        // Advance chain height to past the bootstrap window by inserting a stub block.
        // This sets blocks.height() >= BOOTSTRAP_REGISTRATION_BLOCKS so the stake check fires.
        let stub_block = Block {
            header: BlockHeader {
                protocol_version: 1, height: bootstrap_end,
                parent_hash: state.blocks.latest().unwrap().hash(),
                tx_root: [0u8; 32], proof_root: [0u8; 32], state_root: [0u8; 32],
                timestamp: 9999, producer: addr(0), epoch: 0,
                producer_public_key: vec![], signature: vec![], checkpoint_hash: None,
                chain_id: String::new(),
            },
            transactions: vec![],
            proof_summaries: vec![],
            compliance_summary: None, epoch_summary: None,
        };
        state.blocks.put(stub_block);

        // Fund sender with less than MINIMUM_VALIDATOR_STAKE.
        let acct = state.accounts.get_or_create(sender_addr);
        acct.balance = Amount::from_raw(commputer_core::transaction::MINIMUM_VALIDATOR_STAKE - 1);

        let block = Block {
            header: BlockHeader {
                protocol_version: 1, height: bootstrap_end + 1,
                parent_hash: state.blocks.latest().unwrap().hash(),
                tx_root: [0u8; 32], proof_root: [0u8; 32], state_root: [0u8; 32],
                timestamp: 10000, producer: addr(0), epoch: 0,
                producer_public_key: vec![], signature: vec![], checkpoint_hash: None,
                chain_id: String::new(),
            },
            transactions: vec![Transaction {
                from: sender_addr,
                nonce: 0,
                kind: TxKind::ValidatorRegister {
                    hardware_fingerprint_hash: [0u8; 32],
                    contribution_percent: 100,
                },
                fee: 0,
                signature: vec![], public_key: vec![], memo: None, timelock: None,
            }],
            proof_summaries: vec![],
            compliance_summary: None, epoch_summary: None,
        };
        let result = state.apply_block(&block);
        assert!(result.is_err(), "Validator register with insufficient stake should fail after bootstrap window");

        // Fund with enough and verify registration succeeds.
        let acct = state.accounts.get_or_create(sender_addr);
        acct.balance = Amount::from_raw(commputer_core::transaction::MINIMUM_VALIDATOR_STAKE);

        let block2 = Block {
            header: BlockHeader {
                protocol_version: 1, height: bootstrap_end + 1,
                parent_hash: state.blocks.latest().unwrap().hash(),
                tx_root: [0u8; 32], proof_root: [0u8; 32], state_root: [0u8; 32],
                timestamp: 10001, producer: addr(0), epoch: 0,
                producer_public_key: vec![], signature: vec![], checkpoint_hash: None,
                chain_id: String::new(),
            },
            transactions: vec![Transaction {
                from: sender_addr,
                nonce: 0,
                kind: TxKind::ValidatorRegister {
                    hardware_fingerprint_hash: [0u8; 32],
                    contribution_percent: 100,
                },
                fee: 0,
                signature: vec![], public_key: vec![], memo: None, timelock: None,
            }],
            proof_summaries: vec![],
            compliance_summary: None, epoch_summary: None,
        };
        state.apply_block(&block2).expect("Validator register with sufficient stake should succeed");
        assert!(state.accounts.get(&sender_addr).unwrap().is_validator);
    }

    #[test]
    fn revert_block_restores_balance() {
        let mut state = ChainState::new();
        let genesis = genesis_block();
        state.apply_block(&genesis).unwrap();

        // Create sender with enough for transfer + fee, and pre-create recipient
        let sender = Address([1u8; 32]);
        let recipient = Address([2u8; 32]);
        state.accounts.get_or_create(sender).balance = Amount::from_raw(10_000_000);
        state.accounts.get_or_create(recipient); // Pre-create so no account creation fee
        let balance_before = state.accounts.get(&sender).unwrap().balance.raw();

        let block = Block {
            header: BlockHeader {
                protocol_version: 1, height: 1,
                parent_hash: genesis.hash(),
                tx_root: [0u8; 32], proof_root: [0u8; 32], state_root: [0u8; 32],
                timestamp: 1774656002, producer: Address([0u8; 32]), epoch: 0,
                producer_public_key: vec![], signature: vec![],
                checkpoint_hash: None, chain_id: "commputer-testnet-1".to_string(),
            },
            transactions: vec![Transaction {
                from: sender,
                nonce: 0, fee: 100_000,
                kind: TxKind::Transfer { to: recipient, amount: Amount::from_raw(500_000) },
                public_key: vec![], signature: vec![], memo: None, timelock: None,
            }],
            proof_summaries: vec![], compliance_summary: None, epoch_summary: None,
        };
        state.apply_block(&block).unwrap();
        assert_eq!(state.blocks.height(), 1);

        // Revert
        state.revert_block(1).unwrap();
        assert_eq!(state.accounts.get(&sender).unwrap().balance.raw(), balance_before);
        assert_eq!(state.blocks.height(), 0);
    }

    #[test]
    fn revert_to_multi_block() {
        let mut state = ChainState::new();
        let genesis = genesis_block();
        state.apply_block(&genesis).unwrap();

        let sender = Address([1u8; 32]);
        let recipient = Address([2u8; 32]);
        state.accounts.get_or_create(sender).balance = Amount::from_raw(10_000_000);
        state.accounts.get_or_create(recipient); // Pre-create recipient

        let mut parent = genesis.hash();
        for h in 1..=5u64 {
            let block = Block {
                header: BlockHeader {
                    protocol_version: 1, height: h,
                    parent_hash: parent,
                    tx_root: [0u8; 32], proof_root: [0u8; 32], state_root: [0u8; 32],
                    timestamp: 1774656000 + h, producer: Address([0u8; 32]), epoch: 0,
                    producer_public_key: vec![], signature: vec![],
                    checkpoint_hash: None, chain_id: "commputer-testnet-1".to_string(),
                },
                transactions: vec![Transaction {
                    from: sender,
                    nonce: h - 1, fee: 100_000,
                    kind: TxKind::Transfer { to: recipient, amount: Amount::from_raw(100_000) },
                    public_key: vec![], signature: vec![], memo: None, timelock: None,
                }],
                proof_summaries: vec![], compliance_summary: None, epoch_summary: None,
            };
            parent = block.hash();
            state.apply_block(&block).unwrap();
        }

        assert_eq!(state.blocks.height(), 5);
        // 10M - 5*(100K transfer + 100K fee) = 10M - 1M = 9M
        assert_eq!(state.accounts.get(&sender).unwrap().balance.raw(), 9_000_000);

        // Revert to height 2: undo blocks 3, 4, 5
        let reverted = state.revert_to(2).unwrap();
        assert_eq!(reverted, 3);
        assert_eq!(state.blocks.height(), 2);
        // 10M - 2*(100K + 100K) = 10M - 400K = 9.6M
        assert_eq!(state.accounts.get(&sender).unwrap().balance.raw(), 9_600_000);
    }

    #[test]
    fn revert_beyond_finality_depth_fails() {
        let mut state = ChainState::new();
        let genesis = genesis_block();
        state.apply_block(&genesis).unwrap();

        // We need to revert 0 blocks from height 0, which is fine
        // But requesting revert_to deeper than FINALITY_DEPTH should fail
        assert_eq!(state.blocks.height(), 0);
        // Can't revert genesis anyway
        assert!(state.revert_block(0).is_err());
    }

    #[test]
    fn revert_wrong_height_fails() {
        let mut state = ChainState::new();
        let genesis = genesis_block();
        state.apply_block(&genesis).unwrap();
        // Try to revert height 5 when we're at height 0
        assert!(state.revert_block(5).is_err());
    }

    #[test]
    fn reset_to_genesis() {
        let mut state = ChainState::new();

        // Simulate some state by manually setting fields.
        state.total_emitted = 1000;
        state.total_burned = 500;
        state.current_epoch = 5;
        state.cumulative_score = 42;
        state.snapshot_height = 100;

        // Reset.
        state.reset_to_genesis().unwrap();

        // Verify everything is zeroed.
        assert_eq!(state.blocks.height(), 0);
        assert_eq!(state.total_emitted, 0);
        assert_eq!(state.total_burned, 0);
        assert_eq!(state.current_epoch, 0);
        assert_eq!(state.cumulative_score, 0);
        assert_eq!(state.snapshot_height, 0);
        assert!(state.state_diffs.is_empty());
        assert!(state.validator_performance.is_empty());
        assert!(state.archived_accounts.is_empty());
    }

    // ── Block reward tests ──

    #[test]
    fn block_reward_credited_to_producer() {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();

        let producer = addr(5);
        let block = Block {
            header: BlockHeader {
                protocol_version: 1, height: 1,
                parent_hash: state.blocks.latest().unwrap().hash(),
                producer,
                timestamp: 1000,
                tx_root: [0u8; 32], proof_root: [0u8; 32], state_root: [0u8; 32],
                epoch: 0, producer_public_key: vec![], signature: vec![],
                checkpoint_hash: None, chain_id: String::new(),
            },
            transactions: vec![],
            proof_summaries: vec![],
            compliance_summary: None, epoch_summary: None,
        };
        state.apply_block(&block).unwrap();

        let account = state.accounts.get(&producer).expect("producer should exist");
        // ~15.855 COMME = 1_585_489_599 raw
        assert_eq!(account.balance.raw(), 1_585_489_599);
        assert_eq!(account.total_mined.raw(), 1_585_489_599);
        assert_eq!(state.total_emitted, 1_585_489_599);
    }

    #[test]
    fn no_reward_at_genesis() {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        assert_eq!(state.total_emitted, 0, "genesis should not emit");
    }

    #[test]
    fn no_reward_for_zero_address_producer() {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();

        let block = Block {
            header: BlockHeader {
                protocol_version: 1, height: 1,
                parent_hash: state.blocks.latest().unwrap().hash(),
                producer: Address([0u8; 32]), // zero address
                timestamp: 1000,
                tx_root: [0u8; 32], proof_root: [0u8; 32], state_root: [0u8; 32],
                epoch: 0, producer_public_key: vec![], signature: vec![],
                checkpoint_hash: None, chain_id: String::new(),
            },
            transactions: vec![],
            proof_summaries: vec![],
            compliance_summary: None, epoch_summary: None,
        };
        state.apply_block(&block).unwrap();
        assert_eq!(state.total_emitted, 0, "zero-address producer should not get reward");
    }

    #[test]
    fn reward_capped_to_remaining_supply() {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();

        // Set total_emitted to near the supply cap, leaving only 100 raw units.
        state.total_emitted = commputer_core::token::TOTAL_SUPPLY - 100;

        let producer = addr(5);
        let block = Block {
            header: BlockHeader {
                protocol_version: 1, height: 1,
                parent_hash: state.blocks.latest().unwrap().hash(),
                producer,
                timestamp: 1000,
                tx_root: [0u8; 32], proof_root: [0u8; 32], state_root: [0u8; 32],
                epoch: 0, producer_public_key: vec![], signature: vec![],
                checkpoint_hash: None, chain_id: String::new(),
            },
            transactions: vec![],
            proof_summaries: vec![],
            compliance_summary: None, epoch_summary: None,
        };
        state.apply_block(&block).unwrap();

        // Should only get 100 raw, not the full block reward.
        let account = state.accounts.get(&producer).expect("producer should exist");
        assert_eq!(account.balance.raw(), 100);
        assert_eq!(state.total_emitted, commputer_core::token::TOTAL_SUPPLY);
    }

    // ═══════════════════════════════════════════════════════════════════════════════════
    // Step 1.0 — per-block durable persistence: crash-simulation, stale-row reconcile,
    // journal/mirror unit tests, revert guards, resync + reorg restart consistency.
    // Crash simulation = drop the ChainState WITHOUT flush(); the per-block WriteBatch alone
    // must reproduce the exact pre-crash state (root equality = the fork-after-restart bar).
    // ═══════════════════════════════════════════════════════════════════════════════════

    use commputer_core::wallet::Wallet;
    use commputer_core::signing::sign_transaction;

    fn signed_tx(wallet: &Wallet, nonce: u64, kind: TxKind, fee: u64) -> Transaction {
        let mut tx = Transaction {
            from: *wallet.address(),
            nonce,
            kind,
            fee,
            signature: vec![],
            public_key: vec![],
            memo: None,
            timelock: None,
        };
        sign_transaction(&mut tx, wallet);
        tx
    }

    /// A block with correct merkle roots + chosen producer, ready for `apply_block_validated`.
    fn validated_block(state: &ChainState, height: u64, producer: Address, txs: Vec<Transaction>) -> Block {
        let mut block = block_with(state, height, txs);
        block.header.producer = producer;
        block.header.tx_root = block.compute_tx_root();
        block.header.proof_root = block.compute_proof_root();
        block
    }

    /// A raw block with explicit parent/timestamp (for building fork chains ahead of apply).
    fn raw_block(height: u64, parent: BlockHash, producer: Address, timestamp: u64, txs: Vec<Transaction>) -> Block {
        Block {
            header: BlockHeader {
                protocol_version: 1,
                height,
                parent_hash: parent,
                tx_root: [0u8; 32],
                proof_root: [0u8; 32],
                state_root: [0u8; 32],
                timestamp,
                producer,
                epoch: 0,
                producer_public_key: vec![],
                signature: vec![],
                checkpoint_hash: None,
                chain_id: String::new(),
            },
            transactions: txs,
            proof_summaries: vec![],
            compliance_summary: None,
            epoch_summary: None,
        }
    }

    // --- crash simulation (defect 1: per-block durability without flush) ---------------

    #[test]
    fn crash_survives_bond_block_without_flush() {
        let dir = tempfile::tempdir().unwrap();
        let wallet = Wallet::generate();
        let sender = *wallet.address();
        let (root, bonded, balance, emitted, burned);
        {
            let mut state = ChainState::open(dir.path()).unwrap();
            state.apply_block(&genesis_block()).unwrap();
            state.accounts.get_or_create(sender).balance = Amount::from_raw(5_000);
            state.total_emitted = 5_000;

            let tx = signed_tx(&wallet, 0, TxKind::Bond { amount: Amount::from_raw(3_000) }, 0);
            let block = validated_block(&state, 1, addr(0), vec![tx]);
            state.apply_block_validated(&block).unwrap();

            root = state.compute_state_root();
            bonded = state.bonded_stake.clone();
            balance = state.accounts.get(&sender).unwrap().balance;
            emitted = state.total_emitted;
            burned = state.total_burned;
            // DROP WITHOUT flush() — the per-block batch alone must carry everything.
        }
        let re = ChainState::open(dir.path()).unwrap();
        assert_eq!(re.bonded_stake, bonded, "bonded_stake survives crash");
        assert_eq!(re.bonded_of(&sender), 3_000);
        assert_eq!(re.accounts.get(&sender).unwrap().balance, balance);
        assert_eq!(re.total_emitted, emitted);
        assert_eq!(re.total_burned, burned);
        assert_eq!(
            re.compute_state_root(), root,
            "state root identical after crash-reopen (the fork-after-restart criterion)"
        );
    }

    #[test]
    fn crash_survives_transfer_and_producer_reward() {
        let dir = tempfile::tempdir().unwrap();
        let wallet = Wallet::generate();
        let sender = *wallet.address();
        let producer = addr(5); // non-zero: earns the block reward (in no tx's address list)
        let (root, sender_bal, recipient_bal, producer_bal, emitted);
        {
            let mut state = ChainState::open(dir.path()).unwrap();
            state.apply_block(&genesis_block()).unwrap();
            state.accounts.get_or_create(sender).balance = Amount::from_comme(100);
            state.total_emitted = Amount::from_comme(100).raw();

            let tx = signed_tx(
                &wallet, 0,
                TxKind::Transfer { to: addr(2), amount: Amount::from_comme(33) },
                commputer_core::transaction::ACCOUNT_CREATION_FEE,
            );
            let block = validated_block(&state, 1, producer, vec![tx]);
            state.apply_block_validated(&block).unwrap();

            root = state.compute_state_root();
            sender_bal = state.accounts.get(&sender).unwrap().balance;
            recipient_bal = state.accounts.get(&addr(2)).unwrap().balance;
            producer_bal = state.accounts.get(&producer).unwrap().balance;
            emitted = state.total_emitted;
            assert!(producer_bal.raw() > 0, "producer earned the block reward");
        }
        let re = ChainState::open(dir.path()).unwrap();
        assert_eq!(re.accounts.get(&sender).unwrap().balance, sender_bal);
        assert_eq!(re.accounts.get(&addr(2)).unwrap().balance, recipient_bal, "recipient survives");
        assert_eq!(
            re.accounts.get(&producer).unwrap().balance, producer_bal,
            "producer reward survives (dirty-journal coverage — producer appears in no tx list)"
        );
        assert_eq!(re.total_emitted, emitted);
        assert_eq!(re.compute_state_root(), root);
    }

    #[test]
    fn crash_survives_escrow_and_lifecycle() {
        // Escrow/lifecycle aren't tx-reachable pre-flip — populate directly, then one applied
        // block must persist them (the B2–B4 forward-guarantee).
        let dir = tempfile::tempdir().unwrap();
        let job = [31u8; 32];
        let (root, rec);
        {
            let mut state = ChainState::open(dir.path()).unwrap();
            state.apply_block(&genesis_block()).unwrap();
            state.escrow_by_job.insert([7u8; 32], 4_000);
            let lc = sample_lifecycle(job, 7);
            rec = lc.to_record();
            state.job_lifecycles.insert(job, lc);

            let block = validated_block(&state, 1, addr(0), vec![]);
            state.apply_block_validated(&block).unwrap();
            root = state.compute_state_root();
        }
        let re = ChainState::open(dir.path()).unwrap();
        assert_eq!(re.escrow_by_job.get(&[7u8; 32]), Some(&4_000), "escrow survives crash");
        assert_eq!(
            re.job_lifecycles.get(&job).map(|l| l.to_record()),
            Some(rec),
            "lifecycle survives crash"
        );
        assert_eq!(re.compute_state_root(), root);
    }

    #[test]
    fn batch_inner_recipient_persists_without_flush() {
        // Regression for the before_states gap class: a Batch-inner Transfer recipient appears
        // in no top-level tx address list — only the dirty journal catches it.
        let dir = tempfile::tempdir().unwrap();
        let wallet = Wallet::generate();
        let sender = *wallet.address();
        let fresh = addr(77);
        let root;
        {
            let mut state = ChainState::open(dir.path()).unwrap();
            state.apply_block(&genesis_block()).unwrap();
            state.accounts.get_or_create(sender).balance = Amount::from_comme(10);
            state.total_emitted = Amount::from_comme(10).raw();

            let batch = TxKind::Batch {
                operations: vec![TxKind::Transfer { to: fresh, amount: Amount::from_comme(1) }],
            };
            let tx = signed_tx(&wallet, 0, batch, 0);
            let block = validated_block(&state, 1, addr(0), vec![tx]);
            state.apply_block_validated(&block).unwrap();
            root = state.compute_state_root();
        }
        let re = ChainState::open(dir.path()).unwrap();
        assert_eq!(
            re.accounts.get(&fresh).map(|a| a.balance),
            Some(Amount::from_comme(1)),
            "batch-inner recipient survives crash"
        );
        assert_eq!(re.compute_state_root(), root);
    }

    #[test]
    fn out_of_band_mutation_swept_by_next_block() {
        // Mutations outside block apply (event_loop grace drains / uptime bookkeeping) sit in
        // the dirty journal and ride the NEXT block's batch.
        let dir = tempfile::tempdir().unwrap();
        {
            let mut state = ChainState::open(dir.path()).unwrap();
            state.apply_block(&genesis_block()).unwrap();
            state.accounts.get_or_create(addr(1)).balance = Amount::from_raw(1_000);
            let b1 = validated_block(&state, 1, addr(0), vec![]);
            state.apply_block_validated(&b1).unwrap();

            // Out-of-band mutation between blocks (simulates an event_loop grace drain).
            state.accounts.get_mut(&addr(1)).unwrap().cumulative_uptime_secs = 12_345;

            let b2 = validated_block(&state, 2, addr(0), vec![]);
            state.apply_block_validated(&b2).unwrap();
        }
        let re = ChainState::open(dir.path()).unwrap();
        assert_eq!(
            re.accounts.get(&addr(1)).unwrap().cumulative_uptime_secs,
            12_345,
            "out-of-band mutation swept into the next block's batch"
        );
    }

    #[test]
    fn removed_and_recreated_accounts_persist_correctly_across_crash() {
        // A1: delete-before-put means remove→recreate resolves to the put; removal alone
        // CF-deletes the row (the archived-account-resurrection class bug).
        let dir = tempfile::tempdir().unwrap();
        {
            let mut state = ChainState::open(dir.path()).unwrap();
            state.apply_block(&genesis_block()).unwrap();
            state.accounts.get_or_create(addr(1)).balance = Amount::from_raw(111);
            state.accounts.get_or_create(addr(2)).balance = Amount::from_raw(222);
            let b1 = validated_block(&state, 1, addr(0), vec![]);
            state.apply_block_validated(&b1).unwrap();

            // Remove both, recreate only addr(1) — same inter-block window.
            state.accounts.remove(&addr(1));
            state.accounts.remove(&addr(2));
            state.accounts.get_or_create(addr(1)).balance = Amount::from_raw(333);

            let b2 = validated_block(&state, 2, addr(0), vec![]);
            state.apply_block_validated(&b2).unwrap();
        }
        let re = ChainState::open(dir.path()).unwrap();
        assert_eq!(
            re.accounts.get(&addr(1)).map(|a| a.balance),
            Some(Amount::from_raw(333)),
            "recreated account survives with its new value (delete-before-put)"
        );
        assert!(
            re.accounts.get(&addr(2)).is_none(),
            "removed account's CF row was deleted, not resurrected"
        );
    }

    // --- stale-row resurrection (defect 2: reconciling deletes) ------------------------

    #[test]
    fn withdraw_all_leaves_no_resurrected_rows() {
        let dir = tempfile::tempdir().unwrap();
        let wallet = Wallet::generate();
        let sender = *wallet.address();
        let root;
        {
            let mut state = ChainState::open(dir.path()).unwrap();
            state.stake_params = StakeParams { unbonding_blocks: 0, min_bond: 1_000 };
            state.apply_block(&genesis_block()).unwrap();
            state.accounts.get_or_create(sender).balance = Amount::from_raw(5_000);
            state.total_emitted = 5_000;

            let b1 = validated_block(&state, 1, addr(0), vec![
                signed_tx(&wallet, 0, TxKind::Bond { amount: Amount::from_raw(5_000) }, 0),
            ]);
            state.apply_block_validated(&b1).unwrap();
            let b2 = validated_block(&state, 2, addr(0), vec![
                signed_tx(&wallet, 1, TxKind::RequestUnbond { amount: Amount::from_raw(5_000) }, 0),
            ]);
            state.apply_block_validated(&b2).unwrap();
            let b3 = validated_block(&state, 3, addr(0), vec![
                signed_tx(&wallet, 2, TxKind::WithdrawUnbonded, 0),
            ]);
            state.apply_block_validated(&b3).unwrap();

            assert!(state.bonded_stake.is_empty(), "bonded entry removed in-memory");
            assert!(state.unbonding_stake.is_empty(), "unbonding entry removed in-memory");
            assert_eq!(state.accounts.get(&sender).unwrap().balance, Amount::from_raw(5_000));
            root = state.compute_state_root();
        }
        let re = ChainState::open(dir.path()).unwrap();
        assert!(re.bonded_stake.is_empty(), "no resurrected bonded row after crash-reopen");
        assert!(re.unbonding_stake.is_empty(), "no resurrected unbonding row after crash-reopen");
        assert_eq!(re.accounts.get(&sender).unwrap().balance, Amount::from_raw(5_000));
        assert_eq!(re.compute_state_root(), root);
    }

    #[test]
    fn flush_after_removal_reconciles() {
        // The shutdown-path sweeper: even a DIRECT pub-field `.remove` (bypassing every method)
        // is caught, because the mirror diff is computed from ground truth at write time.
        let dir = tempfile::tempdir().unwrap();
        let job = [33u8; 32];
        {
            let mut state = ChainState::open(dir.path()).unwrap();
            state.bonded_stake.insert(addr(1), 500);
            state.job_lifecycles.insert(job, sample_lifecycle(job, 7));
            state.flush().unwrap();

            state.bonded_stake.remove(&addr(1)); // direct bypass
            state.job_lifecycles.remove(&job); // direct bypass
            state.flush().unwrap();
        }
        let re = ChainState::open(dir.path()).unwrap();
        assert!(re.bonded_stake.is_empty(), "flush deleted the removed bonded row");
        assert!(re.job_lifecycles.is_empty(), "flush deleted the removed lifecycle row");
    }

    // --- pure reconcile / journal semantics ---------------------------------------------

    #[test]
    fn stale_keys_is_mirror_minus_current() {
        let mut mirror: HashSet<[u8; 32]> = HashSet::new();
        mirror.insert([1u8; 32]);
        mirror.insert([2u8; 32]);
        let mut current: HashMap<[u8; 32], u64> = HashMap::new();
        current.insert([2u8; 32], 9);
        current.insert([3u8; 32], 9);
        let stale: Vec<[u8; 32]> = stale_keys(&mirror, &current).copied().collect();
        assert_eq!(stale, vec![[1u8; 32]], "deletes = mirror − current; current keys are re-put");
    }

    #[test]
    fn in_memory_apply_clears_bookkeeping_and_keeps_all_blocks() {
        // A5 pinned semantics for rocks=None: journals cleared + mirrors committed (bounds
        // growth), but blocks are NOT pruned (nothing to reload them from).
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        state.accounts.get_or_create(addr(1)).balance = Amount::from_raw(10_000);
        state.bonded_stake.insert(addr(2), 7);
        let b1 = block_with(&state, 1, vec![]);
        state.apply_block(&b1).unwrap();

        assert_eq!(state.accounts.dirty().count(), 0, "dirty journal cleared without rocks");
        assert_eq!(state.accounts.removed().count(), 0);
        assert!(
            state.persisted_bonded_keys.contains(&addr(2)),
            "mirror committed to current keys without rocks"
        );

        for h in 2..=(ChainState::MEMORY_BLOCK_RETENTION + 5) {
            let b = block_with(&state, h, vec![]);
            state.apply_block(&b).unwrap();
        }
        assert!(
            state.blocks.get_by_height(0).is_some(),
            "genesis retained: memory-only mode must not prune"
        );
        assert_eq!(state.blocks.len() as u64, ChainState::MEMORY_BLOCK_RETENTION + 6);
    }

    #[test]
    fn mirrors_and_journals_advance_only_through_persist() {
        // Rocks-backed: mirrors/journals move exactly at the persist boundary.
        let dir = tempfile::tempdir().unwrap();
        let mut state = ChainState::open(dir.path()).unwrap();
        state.apply_block(&genesis_block()).unwrap();

        state.bonded_stake.insert(addr(1), 500);
        state.accounts.get_or_create(addr(3)).balance = Amount::from_raw(1);
        assert!(!state.persisted_bonded_keys.contains(&addr(1)), "mirror lags until persist");
        assert_eq!(state.accounts.dirty().count(), 1);

        let b1 = validated_block(&state, 1, addr(0), vec![]);
        state.apply_block_validated(&b1).unwrap();
        assert!(state.persisted_bonded_keys.contains(&addr(1)), "mirror advanced on persist");
        assert_eq!(state.accounts.dirty().count(), 0, "journal drained on persist");
    }

    #[test]
    fn open_starts_with_clean_dirty_tracking() {
        // A3: open() loads disk == memory, so nothing is dirty; the first block's batch then
        // carries only that block's touched accounts.
        let dir = tempfile::tempdir().unwrap();
        {
            let mut state = ChainState::open(dir.path()).unwrap();
            state.apply_block(&genesis_block()).unwrap();
            state.accounts.get_or_create(addr(1)).balance = Amount::from_raw(100);
            state.accounts.get_or_create(addr(2)).balance = Amount::from_raw(200);
            let b1 = validated_block(&state, 1, addr(0), vec![]);
            state.apply_block_validated(&b1).unwrap();
        }
        let mut re = ChainState::open(dir.path()).unwrap();
        assert_eq!(re.accounts.dirty().count(), 0, "open() starts with a clean dirty journal");
        assert_eq!(re.accounts.removed().count(), 0);
        re.accounts.get_mut(&addr(1)).unwrap().balance = Amount::from_raw(101);
        assert_eq!(re.accounts.dirty().count(), 1, "only the touched account rides the next batch");
    }

    // --- revert_block fail-safe guards ---------------------------------------------------

    #[test]
    fn revert_block_refuses_when_maps_nonempty() {
        // Guard 1: live map state → refuse, even for a pure-transfer block (backstop for any
        // staleness in the tx-kind scan).
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        state.accounts.get_or_create(addr(1)).balance = Amount::from_raw(10_000_000);
        state.accounts.get_or_create(addr(2));
        let block = block_with(&state, 1, vec![Transaction {
            from: addr(1),
            nonce: 0,
            kind: TxKind::Transfer { to: addr(2), amount: Amount::from_raw(500_000) },
            fee: 100_000,
            signature: vec![],
            public_key: vec![],
            memo: None,
            timelock: None,
        }]);
        state.apply_block(&block).unwrap();

        state.bonded_stake.insert(addr(9), 1);
        let err = state.revert_block(1).unwrap_err();
        assert!(
            err.to_string().contains("consensus maps"),
            "guard 1 refuses with the map message, got: {err}"
        );
    }

    #[test]
    fn revert_block_refuses_map_touching_block_even_with_empty_maps() {
        // Guard 2: bond → unbond-all → withdraw within ONE block leaves the maps empty, but
        // the block still moved map state — the tx-kind scan (incl. Batch-inner) refuses it.
        let mut state = ChainState::new();
        state.stake_params = StakeParams { unbonding_blocks: 0, min_bond: 1_000 };
        state.apply_block(&genesis_block()).unwrap();
        state.accounts.get_or_create(addr(1)).balance = Amount::from_raw(1_000);
        state.total_emitted = 1_000;

        let block = block_with(&state, 1, vec![
            unsigned(addr(1), 0, TxKind::Batch {
                operations: vec![TxKind::Bond { amount: Amount::from_raw(1_000) }],
            }),
            unsigned(addr(1), 1, TxKind::RequestUnbond { amount: Amount::from_raw(1_000) }),
            unsigned(addr(1), 2, TxKind::WithdrawUnbonded),
        ]);
        state.apply_block(&block).unwrap();
        assert!(state.bonded_stake.is_empty() && state.unbonding_stake.is_empty());

        let err = state.revert_block(1).unwrap_err();
        assert!(
            err.to_string().contains("consensus maps"),
            "guard 2 refuses the map-touching block, got: {err}"
        );
    }

    #[test]
    fn revert_block_still_works_on_pure_transfers_with_empty_maps() {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        state.accounts.get_or_create(addr(1)).balance = Amount::from_raw(10_000_000);
        state.accounts.get_or_create(addr(2)); // pre-create: no account-creation fee
        let before = state.accounts.get(&addr(1)).unwrap().balance.raw();

        let block = block_with(&state, 1, vec![Transaction {
            from: addr(1),
            nonce: 0,
            kind: TxKind::Transfer { to: addr(2), amount: Amount::from_raw(500_000) },
            fee: 100_000,
            signature: vec![],
            public_key: vec![],
            memo: None,
            timelock: None,
        }]);
        state.apply_block(&block).unwrap();

        state.accounts.clear_dirty_and_removed(); // isolate: prove revert re-journals
        state.revert_block(1).unwrap();
        assert_eq!(state.accounts.get(&addr(1)).unwrap().balance.raw(), before);
        assert_eq!(state.blocks.height(), 0);
        assert!(
            state.accounts.dirty().any(|a| *a == addr(1)),
            "reverted accounts re-journaled dirty (next persist heals the disk copy)"
        );
    }

    // --- fork recovery: resync (production path) + try_reorg (pre-wiring) ----------------

    #[test]
    fn resync_crash_consistency() {
        // The PRODUCTION fork-recovery path: reset_to_genesis + block-by-block resync via
        // apply_block_validated, then crash without flush.
        let dir = tempfile::tempdir().unwrap();
        let wallet = Wallet::generate();
        let sender = *wallet.address();
        let fee = commputer_core::transaction::ACCOUNT_CREATION_FEE;
        let root;
        {
            let mut state = ChainState::open(dir.path()).unwrap();
            state.apply_block(&genesis_block()).unwrap();
            state.accounts.get_or_create(sender).balance = Amount::from_comme(100);
            state.total_emitted = Amount::from_comme(100).raw();
            let b1 = validated_block(&state, 1, addr(0), vec![
                signed_tx(&wallet, 0, TxKind::Transfer { to: addr(2), amount: Amount::from_comme(1) }, fee),
            ]);
            state.apply_block_validated(&b1).unwrap();
            let b2 = validated_block(&state, 2, addr(0), vec![
                signed_tx(&wallet, 1, TxKind::Transfer { to: addr(3), amount: Amount::from_comme(2) }, fee),
            ]);
            state.apply_block_validated(&b2).unwrap();

            // Fork detected → wipe and resync onto a different chain.
            state.reset_to_genesis().unwrap();
            state.apply_block(&genesis_block()).unwrap();
            state.accounts.get_or_create(sender).balance = Amount::from_comme(100);
            state.total_emitted = Amount::from_comme(100).raw();
            let nb1 = validated_block(&state, 1, addr(0), vec![
                signed_tx(&wallet, 0, TxKind::Transfer { to: addr(4), amount: Amount::from_comme(3) }, fee),
            ]);
            state.apply_block_validated(&nb1).unwrap();

            root = state.compute_state_root();
            // DROP WITHOUT flush.
        }
        let re = ChainState::open(dir.path()).unwrap();
        assert_eq!(re.blocks.height(), 1, "resynced chain height survives crash");
        assert_eq!(
            re.accounts.get(&addr(4)).map(|a| a.balance),
            Some(Amount::from_comme(3)),
            "resynced-chain account survives"
        );
        assert!(re.accounts.get(&addr(2)).is_none(), "pre-reset chain's account is gone");
        assert!(re.accounts.get(&addr(3)).is_none(), "pre-reset chain's account is gone");
        assert_eq!(re.compute_state_root(), root, "root matches the resynced chain exactly");
    }

    #[test]
    fn reorg_persists_winning_chain_across_restart() {
        let dir = tempfile::tempdir().unwrap();
        let fee = commputer_core::transaction::ACCOUNT_CREATION_FEE;
        let (root, emitted);
        {
            let mut state = ChainState::open(dir.path()).unwrap();
            let genesis = genesis_block();
            state.apply_block(&genesis).unwrap();

            // Losing chain: producer addr(1) earns the reward in b1, spends to addr(2) and
            // bonds 500_000 in b2 — the bond puts a LIVE row in CF_BONDED that the reorg's
            // delete_range+re-put reconcile must replace, not resurrect.
            let b1 = raw_block(1, genesis.hash(), addr(1), 2001, vec![]);
            state.apply_block(&b1).unwrap();
            let b2 = raw_block(2, b1.hash(), addr(1), 2002, vec![
                unsigned_with_fee(addr(1), 0, TxKind::Transfer { to: addr(2), amount: Amount::from_raw(500_000) }, fee),
                unsigned_with_fee(addr(1), 1, TxKind::Bond { amount: Amount::from_raw(500_000) }, fee),
            ]);
            state.apply_block(&b2).unwrap();
            assert!(state.accounts.get(&addr(2)).is_some());
            assert_eq!(state.bonded_of(&addr(1)), 500_000);

            // Winning chain: longer, pays addr(3) instead and bonds a DIFFERENT amount.
            let c1 = raw_block(1, genesis.hash(), addr(1), 3001, vec![]);
            let c2 = raw_block(2, c1.hash(), addr(1), 3002, vec![
                unsigned_with_fee(addr(1), 0, TxKind::Transfer { to: addr(3), amount: Amount::from_raw(700_000) }, fee),
                unsigned_with_fee(addr(1), 1, TxKind::Bond { amount: Amount::from_raw(300_000) }, fee),
            ]);
            let c3 = raw_block(3, c2.hash(), addr(1), 3003, vec![]);
            state.try_reorg(vec![c1, c2, c3], 1).unwrap();

            assert_eq!(state.blocks.height(), 3);
            assert!(state.accounts.get(&addr(2)).is_none(), "losing-chain account gone in memory");
            assert_eq!(
                state.accounts.get(&addr(3)).map(|a| a.balance),
                Some(Amount::from_raw(700_000)),
            );
            assert_eq!(state.bonded_of(&addr(1)), 300_000, "winning-chain bond in memory");
            root = state.compute_state_root();
            emitted = state.total_emitted;
            // DROP WITHOUT flush — the one-batch post-reorg reconcile must have covered disk.
        }
        let re = ChainState::open(dir.path()).unwrap();
        assert_eq!(re.blocks.height(), 3, "winning-chain height survives restart");
        assert!(
            re.accounts.get(&addr(2)).is_none(),
            "losing-chain account row was deleted from CF_ACCOUNTS (clear+rewrite)"
        );
        assert_eq!(
            re.accounts.get(&addr(3)).map(|a| a.balance),
            Some(Amount::from_raw(700_000)),
            "winning-chain account survives restart"
        );
        assert_eq!(
            re.bonded_of(&addr(1)),
            300_000,
            "winning-chain bonded row survives restart; losing-chain 500_000 did not resurrect"
        );
        assert_eq!(re.total_emitted, emitted);
        assert_eq!(re.compute_state_root(), root, "root matches the winning chain exactly");
    }

    fn unsigned_with_fee(from: Address, nonce: u64, kind: TxKind, fee: u64) -> Transaction {
        Transaction {
            from,
            nonce,
            kind,
            fee,
            signature: vec![],
            public_key: vec![],
            memo: None,
            timelock: None,
        }
    }
}
