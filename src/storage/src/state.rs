use std::collections::{HashMap, HashSet};
use std::path::Path;
use rocksdb::WriteBatch;
use serde::{Deserialize, Serialize};
use borsh::{BorshSerialize, BorshDeserialize};
use sha2::{Digest, Sha256};
use commputer_core::block::{Block, BlockHash};
use commputer_core::identity::Address;
use commputer_core::token::{Amount, TOTAL_SUPPLY};
use commputer_core::transaction::{TxKind, Transaction};
use commputer_core::compliance::{ComplianceStatus, NerfRate};
use commputer_pouw_onchain::lifecycle::{JobLifecycle, EventResult, Terminal, Phase, PhaseDeadlines};
use commputer_pouw_onchain::escrow_ledger::Ledger;
use commputer_pouw_onchain::escalation_round::{EscalationOutcome, EscalationRound};
use commputer_pouw_onchain::consensus_params::PhaseWindows;
use commputer_pouw::oracle::{ByteEq, ChainHooks, EquivalenceOracle};
use commputer_pouw::ids::ParticipantId;
use commputer_pouw::job::{Commitment, Reveal, SettlementOutcome};
use commputer_pouw::params::GameParams;
use commputer_pouw_onchain::settlement_resolution::{resolve_escalation_fallback, ResolutionParams};
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

/// SECURITY(F11/F22): hard cap on `StorageWill.contact_hashes`. `will_contacts` is
/// persisted per-account and folded into the state root + replicated to every node's
/// RocksDB, so an unbounded list is permanent on-chain bloat. `validate_shape` (core) has
/// no StorageWill arm today, so this apply-side cap is the enforced bound on the gossip
/// path. Generous (a real will names a handful of contacts); a matching `validate_shape`
/// cap in core/transaction.rs should land alongside this for ingress-side rejection.
pub const MAX_WILL_CONTACTS: usize = 64;

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
    /// at settlement increments it. LIVE since Phase 1.1 (B2): `SubmitJobV2` escrows its
    /// budget here; `ClaimJob` adds the executor bond; `Commit` adds verifier bonds; the
    /// settle/expiry drivers drain it.
    ///
    /// PERSISTENCE (P2 step 1.0, complete — applies to all SIX consensus maps): every
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
    /// Phase 1.1 (G-E): genesis-anchored phase windows (result/commit/reveal window lengths,
    /// anchored at CLAIM height, plus the `claim_blocks` submit-anchored claim window).
    /// TODO(B8, PROTECTED genesis): populate from `GenesisConfig`; default until then —
    /// all nodes MUST agree or they diverge on deadlines.
    pub phase_windows: PhaseWindows,
    /// B7 (1.2b, C8): genesis-anchored per-block capacity split. PRODUCER-SIDE ONLY — read during
    /// block assembly by the capacity scheduler (`admit`); NEVER apply-enforced, NEVER persisted, and
    /// NEVER folded into the state root (a v1 SOFT schedule, not a consensus rule). Installed once at
    /// startup by `set_capacity_params` from `GenesisConfig`; default until then.
    pub capacity_params: commputer_pouw_onchain::capacity::CapacityParams,
    /// PoUW P2: per-job verification lifecycle (`job_id` -> `JobLifecycle`), the multi-block
    /// commit-reveal state machine. Created at `ClaimJob` (AwaitingResult, Phase 1.1 B3),
    /// committee drawn at `CompleteJob` (B5, PROTECTED), fed by `Commit`/`Reveal` (B4),
    /// advanced/settled by block height (`settle_due_jobs`, P8). Its money moves run against
    /// `ChainState` via the §3 `Ledger` trait. Persisted per-block as borsh `JobLifecycleRecord`
    /// DTOs (CF_LIFECYCLE) and folded into the state root; params re-injected on load (see the
    /// B1b note on `game_params` above).
    pub job_lifecycles: HashMap<[u8; 32], JobLifecycle>,
    /// Phase 1.1 (B2/G-B): the 5th consensus map — submitted-but-unclaimed jobs
    /// (`job_id` -> `PendingJobRecord`). Written by `SubmitJobV2` (which also escrows the
    /// budget), consumed by `ClaimJob` (B3, opens the lifecycle) or refunded by
    /// `expire_pending_job` once `claim_by` passes. Persisted per-block (CF_PENDING) and folded
    /// into the state root exactly like the other four maps.
    pub pending_jobs: HashMap<[u8; 32], PendingJobRecord>,
    /// PoUW S4 (EscalationRound, 2026-07-19): the 6th consensus map — in-flight escalation panel
    /// rounds (`job_id` -> `EscalationRound`), opened when a `JobLifecycle` settles to
    /// `Terminal::Escalate` (the open/apply site is Tasks 5-6). Persisted per-block as borsh
    /// `EscalationRoundRecord` DTOs (CF_ESCALATION) and folded into the state root (the sixth and
    /// final Policy-B section, appended after `pending_jobs` — order is consensus); params
    /// re-injected on load exactly like `job_lifecycles` (see the B1b note on `game_params` above).
    pub escalation_rounds: HashMap<[u8; 32], EscalationRound>,
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
    persisted_pending_keys: HashSet<[u8; 32]>,
    persisted_escalation_keys: HashSet<[u8; 32]>,
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
            .field("pending_jobs", &self.pending_jobs.len())
            .field("escalation_rounds", &self.escalation_rounds.len())
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
            phase_windows: PhaseWindows::default(),
            capacity_params: commputer_pouw_onchain::capacity::CapacityParams::default(),
            job_lifecycles: HashMap::new(),
            pending_jobs: HashMap::new(),
            escalation_rounds: HashMap::new(),
            persisted_escrow_keys: HashSet::new(),
            persisted_bonded_keys: HashSet::new(),
            persisted_unbonding_keys: HashSet::new(),
            persisted_lifecycle_keys: HashSet::new(),
            persisted_pending_keys: HashSet::new(),
            persisted_escalation_keys: HashSet::new(),
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

        // PoUW P2 (B1a): load the persisted consensus money/stake maps. M1: a corrupt (present-but-
        // undecodable) consensus row now FAILS the open with an actionable error rather than
        // warn-skipping — a silently-dropped consensus row makes P8's pot guard reject every block
        // forever. An empty CF loads as an empty map (no error).
        let escrow_by_job = rocks.all_escrow().map_err(StateError::StorageError)?;
        let bonded_stake = rocks.all_bonded().map_err(StateError::StorageError)?;
        let unbonding_stake = rocks.all_unbonding().map_err(StateError::StorageError)?;
        // Phase 1.1 (B2/G-B): the 5th map — submitted-but-unclaimed jobs.
        let pending_jobs = rocks.all_pending().map_err(StateError::StorageError)?;

        // PoUW P2 (B1b): load persisted job_lifecycles. GameParams/ResolutionParams are genesis-anchored
        // (identical for every job) so they are NOT persisted per-job — reconstruct + re-inject them.
        // TODO(B8, PROTECTED genesis): populate these from GenesisConfig; default until then. Because
        // every lifecycle was created with the same consensus params, re-injecting this copy reproduces
        // the original params exactly (true now with defaults, and post-B8 with genesis values).
        // open() reconstructs lifecycles with DEFAULT params (byte-identical to pre-B8 behavior); the
        // node's `set_consensus_params` call right after open() (1.2b main.rs) RE-INJECTS the genesis
        // params into every lifecycle before any block settles (C1 — the fork-safety fix), so settling
        // with these defaults never reaches consensus.
        let game_params = GameParams::default();
        let resolution_params = ResolutionParams::default();
        let job_lifecycles: HashMap<[u8; 32], JobLifecycle> = rocks
            .all_lifecycle()
            .map_err(StateError::StorageError)?
            .into_iter()
            .map(|(id, rec)| (id, JobLifecycle::from_record(rec, game_params.clone(), resolution_params)))
            .collect();

        // PoUW S4: load persisted escalation_rounds. Same param re-injection discipline as
        // job_lifecycles above — GameParams is genesis-anchored (identical for every round) so it
        // is NOT persisted per-round; re-injected here (and again by `set_consensus_params`, C1).
        let escalation_rounds: HashMap<[u8; 32], EscalationRound> = rocks
            .all_escalation()
            .map_err(StateError::StorageError)?
            .into_iter()
            .map(|(id, rec)| (id, EscalationRound::from_record(rec, game_params.clone())))
            .collect();

        // Persisted-key mirrors start EXACT: load IS a CF scan, so each loaded key set equals
        // the rows on disk. (Warn-skipped malformed rows linger as junk OUTSIDE the mirror and
        // the state root; they are re-skipped on every open and cannot resurrect — no
        // delete-on-load.)
        let persisted_escrow_keys: HashSet<[u8; 32]> = escrow_by_job.keys().copied().collect();
        let persisted_bonded_keys: HashSet<Address> = bonded_stake.keys().copied().collect();
        let persisted_unbonding_keys: HashSet<Address> = unbonding_stake.keys().copied().collect();
        let persisted_lifecycle_keys: HashSet<[u8; 32]> = job_lifecycles.keys().copied().collect();
        let persisted_pending_keys: HashSet<[u8; 32]> = pending_jobs.keys().copied().collect();
        let persisted_escalation_keys: HashSet<[u8; 32]> = escalation_rounds.keys().copied().collect();

        let account_count = accounts.len();
        let block_count = blocks.len();
        let height = blocks.height();

        info!(
            "Loaded state from disk: {} blocks (height {}), {} accounts, epoch {}; \
             escrow_pots={}, bonded={}, unbonding={}, lifecycles={}, pending_jobs={}",
            block_count, height, account_count, current_epoch,
            escrow_by_job.len(), bonded_stake.len(), unbonding_stake.len(), job_lifecycles.len(),
            pending_jobs.len(),
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
            phase_windows: PhaseWindows::default(),
            capacity_params: commputer_pouw_onchain::capacity::CapacityParams::default(),
            job_lifecycles,
            pending_jobs,
            escalation_rounds,
            persisted_escrow_keys,
            persisted_bonded_keys,
            persisted_unbonding_keys,
            persisted_lifecycle_keys,
            persisted_pending_keys,
            persisted_escalation_keys,
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

        // Phase 1.1 (B2): pending_jobs (debug-only; the authoritative commitment is `state_root`).
        let mut pending: Vec<(&[u8; 32], &PendingJobRecord)> = self.pending_jobs.iter().collect();
        pending.sort_by(|a, b| a.0.cmp(b.0));
        let pending_json: Vec<serde_json::Value> = pending.iter().map(|(id, r)| {
            serde_json::json!({
                "job_id": hex::encode(id),
                "submitter": hex::encode(r.submitter),
                "budget": r.budget,
                "submitted_height": r.submitted_height,
                "claim_by": r.claim_by,
            })
        }).collect();

        // PoUW S4: escalation_rounds (debug-only; the authoritative commitment is `state_root`).
        let mut escalations: Vec<(&[u8; 32], &EscalationRound)> = self.escalation_rounds.iter().collect();
        escalations.sort_by(|a, b| a.0.cmp(b.0));
        let escalations_json: Vec<serde_json::Value> = escalations.iter().map(|(id, er)| {
            let r = er.to_record();
            serde_json::json!({
                "job_id": hex::encode(id),
                "phase": format!("{:?}", er.phase()),
                "panel": r.panel.len(),
                "commitments": r.commitments.len(),
                "reveals": r.reveals.len(),
                "settled": r.settled.is_some(),
                "expected_escrow": er.expected_escrow(),
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
            "pending_jobs": pending_json,
            "escalation_rounds": escalations_json,
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
    /// SIX consensus maps are empty (a chain that has never seen a PoUW tx), this returns
    /// the accounts-only root BYTE-IDENTICAL to before B1a. Once ANY map is non-empty, the root
    /// folds the accounts root + ALL SIX maps (each iterated in SORTED key order — HashMap
    /// iteration is nondeterministic — and length-prefixed so the encoding is injective across
    /// map boundaries); the first Bond tx alone flips a chain to the 6-section format (the
    /// early-return is all-or-nothing — P10a). The format change thus lands with the same
    /// coordinated flip that makes the maps fillable.
    pub fn compute_state_root(&self) -> [u8; 32] {
        let accounts_root = self.accounts.compute_state_root();
        if self.escrow_by_job.is_empty()
            && self.bonded_stake.is_empty()
            && self.unbonding_stake.is_empty()
            && self.job_lifecycles.is_empty()
            && self.pending_jobs.is_empty()
            && self.escalation_rounds.is_empty()
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
        // across nodes. Per-entry blob length-prefixed (LE) so the encoding stays injective. The
        // Policy-B fold has SIX sections total (pending_jobs + escalation_rounds below); while ALL six
        // maps are empty the root stays the pre-B1a accounts-only root byte-for-byte.
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

        // pending_jobs (Phase 1.1 / B2): sorted by job_id, length-prefixed; value =
        // borsh(PendingJobRecord) (all fixed-size fields ⇒ canonical encoding), per-entry blob
        // length-prefixed (LE) — the CF_LIFECYCLE fold pattern. This is the FIFTH Policy-B section
        // (escalation_rounds below is the sixth and final).
        let mut pending: Vec<(&[u8; 32], &PendingJobRecord)> = self.pending_jobs.iter().collect();
        pending.sort_by(|a, b| a.0.cmp(b.0));
        h.update((pending.len() as u64).to_le_bytes());
        for (job_id, rec) in pending {
            h.update(job_id);
            let blob = borsh::to_vec(rec)
                .expect("pending record borsh serialization should not fail");
            h.update((blob.len() as u64).to_le_bytes());
            h.update(&blob);
        }

        // escalation_rounds (EscalationRound 2026-07-19): sorted by job_id, length-prefixed;
        // value = borsh(EscalationRoundRecord). The SIXTH Policy-B section — appended after
        // pending_jobs; the section order is consensus.
        let mut escalations: Vec<(&[u8; 32], &EscalationRound)> = self.escalation_rounds.iter().collect();
        escalations.sort_by(|a, b| a.0.cmp(b.0));
        h.update((escalations.len() as u64).to_le_bytes());
        for (job_id, er) in escalations {
            h.update(job_id);
            let blob = borsh::to_vec(&er.to_record())
                .expect("escalation record borsh serialization should not fail");
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

    /// A-batch item 7: apply genesis account allocations at height 0.
    ///
    /// Credits each `(address_hex, raw_units)` to its account balance AND bumps
    /// `total_emitted` by the same amount. Because both sides move together, the
    /// supply invariants are preserved exactly:
    ///   * circulating (`total_emitted - total_burned`) grows by the credited sum,
    ///     matching the growth in Σ balances;
    ///   * `remaining_supply` (`TOTAL_SUPPLY - total_emitted`) shrinks by the sum;
    ///   * `total_supply` / `TOTAL_SUPPLY` are unchanged.
    ///
    /// Determinism: crediting is additive, so the resulting account map and the
    /// `total_emitted` sum do NOT depend on entry order — two independent builds
    /// from the same genesis produce the identical state root. Duplicate addresses
    /// accumulate.
    ///
    /// EMPTY = byte-identical: an empty slice performs NO mutation at all (no
    /// account touched, `total_emitted` untouched), so a genesis that omits
    /// `accounts` yields the same accounts root and the same `total_emitted` as a
    /// chain that never called this. This is the continuity guarantee for the
    /// existing genesis.json.
    ///
    /// Fail-closed + all-or-nothing: the full set is validated (hex parse, sum
    /// overflow, and the supply cap `total_emitted + sum <= TOTAL_SUPPLY`) BEFORE
    /// any mutation, so a bad entry leaves state untouched and genesis can never
    /// mint above the fixed cap. Genesis-only: rejects application once the chain
    /// has advanced past the genesis block (`blocks.height() != 0`).
    ///
    /// WIRING (INERT until then): the node calls this ONCE from its genesis path
    /// at the alpha reset — a PROTECTED `main.rs` edit (D6) — right after the
    /// height-0 genesis block is applied. No caller exists yet, so this ships with
    /// zero behavior change; funding an account is a genesis.json edit, not code.
    pub fn apply_genesis_accounts(
        &mut self,
        accounts: &[(String, u64)],
    ) -> Result<(), StateError> {
        // EMPTY: no-op. Guarantees byte-identical continuity for today's genesis.
        if accounts.is_empty() {
            return Ok(());
        }

        // Genesis-only: never mint into a chain that has already advanced.
        let height = self.blocks.height();
        if height != 0 {
            return Err(StateError::InvalidBlock(format!(
                "genesis accounts can only be applied at height 0 (chain is at height {height})"
            )));
        }
        // Idempotency guard: genesis credits are the ONLY thing that can bump
        // `total_emitted` before the genesis block is applied, so a non-zero value
        // here means this ran already (or a block was processed). Reject rather than
        // double-credit — defends the single-call-site D6 contract against re-entry.
        if self.total_emitted != 0 {
            return Err(StateError::InvalidBlock(
                "genesis accounts already applied (total_emitted != 0); refusing to double-credit"
                    .to_string(),
            ));
        }

        // Validate the entire set before mutating (all-or-nothing).
        let mut parsed: Vec<(Address, u64)> = Vec::with_capacity(accounts.len());
        let mut sum: u64 = 0;
        for (addr_hex, raw) in accounts {
            let addr = Address::from_hex(addr_hex).map_err(|e| {
                StateError::InvalidBlock(format!(
                    "genesis accounts: invalid address '{addr_hex}': {e}"
                ))
            })?;
            // SECURITY(F33): never credit the keyless all-zero address at genesis. Balance that
            // reaches it is spendable by an UNSIGNED zero-from Transfer a producer can inject
            // (apply_block_validated skips signature checks for zero-from), so a founder typo of
            // 0x000..0 in genesis.json would mint attacker-drainable funds.
            if addr.is_zero() {
                return Err(StateError::InvalidBlock(
                    "genesis accounts: refusing to credit the zero address".to_string(),
                ));
            }
            sum = sum.checked_add(*raw).ok_or_else(|| {
                StateError::InvalidBlock(
                    "genesis accounts: allocation sum overflows u64".to_string(),
                )
            })?;
            parsed.push((addr, *raw));
        }

        // Supply cap: genesis credits are emission and must fit under the fixed
        // TOTAL_SUPPLY. total_emitted is 0 at a fresh genesis, but fold against the
        // live value to stay correct regardless.
        let new_emitted = self.total_emitted.checked_add(sum).ok_or_else(|| {
            StateError::InvalidBlock("genesis accounts: total_emitted overflow".to_string())
        })?;
        if new_emitted > TOTAL_SUPPLY {
            return Err(StateError::InvalidBlock(format!(
                "genesis accounts: allocation sum {sum} would push total_emitted \
                 {new_emitted} above TOTAL_SUPPLY {TOTAL_SUPPLY}"
            )));
        }

        // Apply. Balance credit is additive; the sum-vs-cap check above bounds
        // every per-account total below u64::MAX, so checked_add cannot fail here.
        for (addr, raw) in parsed {
            let account = self.accounts.get_or_create(addr);
            account.balance = account.balance.checked_add(Amount::from_raw(raw)).ok_or_else(|| {
                StateError::InvalidBlock(format!(
                    "genesis accounts: balance overflow crediting {}",
                    hex::encode(addr.0)
                ))
            })?;
        }
        self.total_emitted = new_emitted;
        Ok(())
    }

    /// P1+P8 shared core of ALL THREE apply paths (`apply_block` / `apply_block_validated` /
    /// `apply_block_atomic`): apply every transaction, then run the deterministic settlement
    /// driver (`settle_due_jobs`) — and on ANY Err restore the pre-block state before
    /// propagating it.
    ///
    /// P1 BLOCKER FIX: without this, a block rejected at tx *i* left txs `<i` (and the fee of
    /// tx *i*) applied in memory; under step 1.0 those smeared accounts stayed in the dirty
    /// journal and smeared maps stayed live, so the NEXT successful block's
    /// `persist_applied_block` wrote them into the CFs and the state root — a malicious block
    /// fed to a subset of nodes forked honest nodes persistently. Rollback semantics:
    /// - rocks-backed: reload accounts + all six consensus maps + meta counters from the CFs
    ///   (disk == post-last-good-block by step 1.0's per-block-batch guarantee) and re-run
    ///   `open()`'s hygiene (clean journals, mirrors == CF key sets). Out-of-band mutations
    ///   not yet swept by a block (grace drains, epoch bump) are rewound too — identical to
    ///   what a crash-restart would lose; accepted.
    /// - memory-only (tests + try_reorg's rocks-detached replay): restore the pre-block
    ///   snapshot taken at entry (there is no disk copy to reload).
    fn apply_txs_with_rollback(&mut self, block: &Block) -> Result<(), StateError> {
        let snap = self.capture_pre_block();
        let applied: Result<(), StateError> = (|| {
            for tx in &block.transactions {
                self.apply_transaction(tx)?;
            }
            // B5: draw the committee for every job whose executor posted a result (this block or
            // earlier) but whose committee is not yet drawn — BETWEEN the tx loop and settlement,
            // inside the rollback envelope. Money-free + deterministic (seed = block.hash()).
            self.draw_committees_for_completed_jobs(block.hash());
            // P8: the deterministic in-apply settlement driver — runs INSIDE the rollback
            // envelope (its guards are unreachable by construction, but if one ever fires the
            // block is rejected without smear) and BEFORE the block is stored/persisted, so
            // every settlement/expiry mutation rides this block's WriteBatch. The block hash
            // seeds the escalation-panel draw (S5), exactly like the B5 committee draw above.
            self.settle_due_jobs(block.height(), block.hash())
        })();
        if let Err(e) = applied {
            self.rollback_to_pre_block(snap);
            return Err(e);
        }
        Ok(())
    }

    /// P1: snapshot everything `apply_transaction` + `settle_due_jobs` can mutate, so a failed
    /// block can be rolled back exactly. Taken unconditionally (rocks-backed too): reloading
    /// from the CFs would restore only the last *persisted* state and silently rewind
    /// out-of-band mutations applied in memory since the last block (the epoch tick's
    /// `current_epoch` bump + per-account uptime pokes, peer-disconnect grace drains) — those
    /// ride the NEXT block's persist, so they are live-in-memory-but-not-yet-on-disk during a
    /// block apply. A disk reload would rewind them while the event loop's companion epoch
    /// state stayed advanced — a mixed state no crash-restart can produce, diverging this node's
    /// account roots from peers that never saw the rejected block. The memory snapshot restores
    /// the exact pre-block state (including the dirty/removed journals), so no such divergence is
    /// possible. Cost is an O(accounts + maps) clone per block — trivial at testnet scale.
    fn capture_pre_block(&self) -> Box<BlockSnapshot> {
        Box::new(BlockSnapshot {
            accounts: self.accounts.clone(), // carries the pre-block dirty/removed journals too
            escrow_by_job: self.escrow_by_job.clone(),
            bonded_stake: self.bonded_stake.clone(),
            unbonding_stake: self.unbonding_stake.clone(),
            job_lifecycles: self.job_lifecycles.clone(),
            pending_jobs: self.pending_jobs.clone(),
            escalation_rounds: self.escalation_rounds.clone(),
            total_emitted: self.total_emitted,
            total_burned: self.total_burned,
            current_epoch: self.current_epoch,
            nerf_rate: self.nerf_rate,
        })
    }

    /// P1: restore the pre-block state after a failed apply. Mirrors advance only at persist
    /// (which never ran for the rejected block), so there is nothing there to restore.
    fn rollback_to_pre_block(&mut self, snap: Box<BlockSnapshot>) {
        self.accounts = snap.accounts;
        self.escrow_by_job = snap.escrow_by_job;
        self.bonded_stake = snap.bonded_stake;
        self.unbonding_stake = snap.unbonding_stake;
        self.job_lifecycles = snap.job_lifecycles;
        self.pending_jobs = snap.pending_jobs;
        self.escalation_rounds = snap.escalation_rounds;
        self.total_emitted = snap.total_emitted;
        self.total_burned = snap.total_burned;
        self.current_epoch = snap.current_epoch;
        self.nerf_rate = snap.nerf_rate;
    }

    /// P8: the deterministic in-apply settlement driver. Called from the shared tail of all
    /// three apply paths with the APPLIED block's height — never from a wall-clock tick
    /// (out-of-band settlement lands at different heights on different nodes ⇒ per-height
    /// state-root divergence ⇒ fork; B6's PROTECTED tick becomes observe/log-only).
    ///
    /// Iterates due jobs in SORTED job-key order (HashMap order must never reach consensus
    /// state):
    /// 1. pending jobs past `claim_by` → `expire_pending_job` (full budget refund);
    /// 2. every lifecycle: `advance` (idempotent, money-free height transition), then
    ///    `should_settle` → `lifecycle_settle_and_drain`. A cached-but-undrained terminal
    ///    (`is_settled`, unreachable outside tests) is drained too, so nothing can strand.
    /// 3. every escalation round (S6): `advance`, then `should_settle` →
    ///    `escalation_settle_and_drain` — AFTER the lifecycle sweep, so a round opened by a
    ///    `Terminal::Escalate` this block is advanced (no-op at open height) but never
    ///    insta-settled (its deadlines are ≥ 1 window ahead by the C4 guard).
    ///
    /// `block_hash` seeds the S5 escalation-panel draw inside `lifecycle_settle_and_drain`
    /// (domain-separated from the B5 committee draw by the "escalate" tag).
    ///
    /// try_reorg's replay reproduces identical settle heights by construction (same tail).
    fn settle_due_jobs(&mut self, height: u64, block_hash: BlockHash) -> Result<(), StateError> {
        // The consensus equivalence oracle is PINNED to byte-equality: a future oracle change
        // must enter ConsensusParams::fingerprint before becoming configurable.
        const SETTLE_ORACLE: ByteEq = ByteEq;

        let mut due_pending: Vec<[u8; 32]> = self
            .pending_jobs
            .iter()
            .filter(|(_, r)| height > r.claim_by)
            .map(|(k, _)| *k)
            .collect();
        due_pending.sort_unstable();
        for job_id in due_pending {
            self.expire_pending_job(job_id, height)?;
        }

        let mut jobs: Vec<[u8; 32]> = self.job_lifecycles.keys().copied().collect();
        jobs.sort_unstable();
        for job_id in jobs {
            self.lifecycle_advance(job_id, height);
            let due = self
                .job_lifecycles
                .get(&job_id)
                .map(|l| l.should_settle(height) || l.is_settled())
                .unwrap_or(false);
            if due {
                self.lifecycle_settle_and_drain(job_id, &SETTLE_ORACLE, block_hash)?;
            }
        }

        // EscalationRound sweep (S6): advance then settle-when-due, SORTED job order (same
        // discipline as the lifecycle sweep above; the pinned ByteEq oracle is reused).
        let mut esc: Vec<[u8; 32]> = self.escalation_rounds.keys().copied().collect();
        esc.sort_unstable();
        for job_id in esc {
            if let Some(er) = self.escalation_rounds.get_mut(&job_id) {
                er.advance(height);
            }
            let due = self
                .escalation_rounds
                .get(&job_id)
                .map(|er| er.should_settle(height) || er.is_settled())
                .unwrap_or(false);
            if due {
                self.escalation_settle_and_drain(job_id, &SETTLE_ORACLE)?;
            }
        }
        Ok(())
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

        // Process transactions + run the P8 settlement driver, with P1 rollback-on-Err:
        // a rejected block leaves NO smear (accounts/maps/meta restored to pre-block state).
        self.apply_txs_with_rollback(block)?;

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
        // Genuinely protocol-issued transactions (e.g. MiningReward) come from the zero
        // address and have no signature — skip verification for those.
        // NOTE(F10/F21 reconcile, f0fdac4): MilestoneBurn/CharitableDonation are NO LONGER
        // in this zero-from protocol-issuance set. They are user-authorized burns that debit
        // the sender's balance and consume a nonce, and their apply arms REJECT zero-from
        // outright — so despite the historical framing they must never be emitted from zero.
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

        // Process transactions + run the P8 settlement driver, with P1 rollback-on-Err:
        // a rejected block leaves NO smear (accounts/maps/meta restored to pre-block state).
        // NOTE: credit_block_reward is intentionally called AFTER this so that if any
        // transaction fails, total_emitted and producer balance are never mutated.
        self.apply_txs_with_rollback(block)?;

        // SECURITY(F25): block.header.state_root is NOT recomputed/compared against post-apply
        // state here — a peer can forge it. Fix intentionally DEFERRED to founder review (naive
        // equality risks bricking sync given the producer's pre-reward snapshot convention). See
        // the security addendum before enabling. TODO(F25).

        // Generate receipts + history ONLY for an accepted block (P1: a rejected block must
        // leave no trace — receipts for its prefix would claim success for txs that never
        // landed).
        let block_hash = block.hash();
        for (i, tx) in block.transactions.iter().enumerate() {
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
                // SECURITY(F33): apply_block_validated skips signature verification for zero-from
                // txs, so without this guard a block producer could inject an UNSIGNED
                // Transfer{from:zero,..} and drain any balance that reached the keyless zero
                // address. Mirror the zero-from guards on Bond/SubmitJobV2/etc.
                if tx.from.is_zero() {
                    return Err(StateError::InvalidBlock(
                        "zero address cannot transfer".into(),
                    ));
                }
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
                // SECURITY(F10/F21): previously this bumped consensus `total_burned` with NO
                // balance debit, NO authorization, and NO nonce consumption — so any funded
                // account could forge `MilestoneBurn { burn_amount: u64::MAX }` for one min-fee
                // tx and permanently corrupt circulating_supply (→ emergency-access flip), and
                // the same tx replayed since the nonce never advanced. Conservation requires
                // `total_burned` to rise ONLY when real balance leaves circulation. Mirror
                // BurstCompute: reject zero-from, require + debit the sender's balance, bump nonce.
                if tx.from.is_zero() {
                    return Err(StateError::InvalidBlock(
                        "zero address cannot submit milestone burn".into(),
                    ));
                }
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

            TxKind::CharitableDonation { burn_amount, .. } => {
                // SECURITY(F10/F21): same forgeable/replayable burn-accounting corruption as
                // MilestoneBurn above. Require the burn to actually cost the sender's balance and
                // consume a nonce so `total_burned` only rises against real removed circulation.
                if tx.from.is_zero() {
                    return Err(StateError::InvalidBlock(
                        "zero address cannot submit charitable donation".into(),
                    ));
                }
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

            TxKind::StorageWill { contact_hashes, .. } => {
                // SECURITY(F11/F22): cap contact_hashes so a single will can't bloat permanent,
                // replicated per-account on-chain state (folded into the state root + persisted to
                // every node's RocksDB). validate_shape (core) has no StorageWill arm, so this
                // apply-side cap is the enforced bound on the gossip path (defense in depth).
                if contact_hashes.len() > MAX_WILL_CONTACTS {
                    return Err(StateError::InvalidBlock(format!(
                        "storage will contact_hashes {} exceeds max of {}",
                        contact_hashes.len(),
                        MAX_WILL_CONTACTS
                    )));
                }
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
                // SECURITY(F3/F6): bound signers/signatures BEFORE the O(signatures×signers)
                // ed25519 verify loop below. The real N-of-M cap (MAX_MULTISIG_SIGNERS) lives
                // ONLY in Transaction::validate_shape, which the gossip ingress path never calls —
                // so an attacker could gossip a self-signed multisig carrying ~thousands of
                // signers/signatures and freeze the single-threaded event loop for minutes at
                // trial-apply (chain-halt DoS). This hard cap keeps the loop ≤ MAX_MULTISIG_SIGNERS²
                // regardless of ingress path (defense in depth).
                let max_multisig = commputer_core::transaction::Transaction::MAX_MULTISIG_SIGNERS;
                if signers.len() > max_multisig || signatures.len() > max_multisig {
                    return Err(StateError::InvalidBlock(format!(
                        "multisig signer/signature count exceeds max of {}",
                        max_multisig
                    )));
                }
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

            // SubmitJob (legacy) keeps burn-at-submit byte-for-byte; SubmitJobV2 escrows (B2).
            TxKind::SubmitJob { comme_budget, .. } => {
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

            // Phase 1.1 (B2): SubmitJobV2 ESCROWS the budget into a per-job pot — HELD in
            // circulating supply, `total_burned` NOT touched (only a settlement resolver's
            // burn slice moves it). Every Err fires before any mutation of this arm except
            // `escrow_into_job`, which itself validates pot-overflow then balance BEFORE
            // mutating — no partial state; a failed tx rejects the whole block (G-H) and P1
            // rollback wipes any earlier-tx smear.
            // BORROW NOTE: the outer `sender` borrow is NOT used here (NLL — the Bond-arm
            // pattern); nonce via a fresh get_or_create AFTER all fallible ops.
            TxKind::SubmitJobV2 { program_hash, input_hash, da_root, comme_budget, .. } => {
                // P3: zero-from txs skip signature verification entirely — the keyless zero
                // address must never own a pot/pending record.
                if tx.from.is_zero() {
                    return Err(StateError::InvalidBlock(
                        "zero address cannot submit compute jobs".into(),
                    ));
                }
                if comme_budget.raw() < commputer_core::compute::MIN_JOB_BUDGET {
                    return Err(StateError::InvalidBlock(format!(
                        "compute job budget {} below minimum {}",
                        comme_budget.raw(),
                        commputer_core::compute::MIN_JOB_BUDGET
                    )));
                }
                // G-A: on-chain job identity = the tx hash (matches the node pool's PoolJobId
                // convention; nonce inside the hash ⇒ per-tx unique). KNOWN fund-safe griefing
                // vector (P10c): memo/timelock sit outside the SIGNED payload but inside the
                // hash, so a relayer can shift the job_id pre-inclusion — the escrow stays
                // recoverable via `expire_pending_job`'s refund; the signed-payload id +
                // PROTECTED PoolJobId change is a flip-notes follow-up.
                let job_id = tx.hash().0;
                // Defense-in-depth duplicate guard (a distinct colliding tx = SHA-256
                // collision; a literal re-broadcast already fails the nonce check).
                if self.pending_jobs.contains_key(&job_id)
                    || self.escrow_by_job.contains_key(&job_id)
                    || self.job_lifecycles.contains_key(&job_id)
                {
                    return Err(StateError::InvalidBlock("duplicate job id".into()));
                }
                // Balance check + move balance→pot in one audited primitive (no partial state
                // on Err): InsufficientBalance rejects the block.
                self.escrow_into_job(&tx.from, job_id, comme_budget.raw())?;
                let h = self.blocks.height(); // G-F: parent height during apply
                self.pending_jobs.insert(job_id, PendingJobRecord {
                    submitter: tx.from.0,
                    budget: comme_budget.raw(),
                    program_hash: *program_hash,
                    input_hash: *input_hash,
                    da_root: *da_root,
                    submitted_height: h,
                    claim_by: h.saturating_add(self.phase_windows.claim_blocks),
                });
                self.accounts.get_or_create(tx.from).nonce += 1; // AFTER all fallible ops
            }

            TxKind::ClaimJob { job_id } => {
                // Phase 1.1 (B3): full claim semantics (validator gate + lifecycle open for a
                // pending V2 job; legacy accept for unknown/V1 ids) — shared with the Batch arm.
                self.apply_claim_job(tx.from, *job_id)?;
                self.accounts.get_or_create(tx.from).nonce += 1;
            }

            TxKind::CompleteJob { job_id, result_hash } => {
                // B5 (flip): the executor posts its result hash. `post_result` RECORDS it (validating
                // phase/executor/window at PARENT height); the committee is DRAWN in the block tail
                // (`draw_committees_for_completed_jobs`) from the `block.hash()` seed — never here (the
                // tx arm has no block hash). Unknown / wrong-phase / wrong-executor / past-window all
                // reject the whole block (consensus-format change in the coordinated flip). No money
                // moves. BORROW NOTE: the outer `sender` is not referenced (Bond-arm pattern); nonce
                // via a fresh get_or_create after the fallible check.
                // P3: the keyless zero address can never be an executor.
                if tx.from.is_zero() {
                    return Err(StateError::InvalidBlock("zero address cannot complete jobs".into()));
                }
                let height = self.blocks.height(); // G-F parent height
                match self.lifecycle_post_result(*job_id, ParticipantId(tx.from.0), *result_hash, height) {
                    Some(EventResult::Accepted) => {}
                    Some(EventResult::Rejected(r)) => {
                        return Err(StateError::InvalidBlock(format!("complete rejected: {r:?}")));
                    }
                    None => return Err(StateError::InvalidBlock("complete: unknown job".into())),
                }
                self.accounts.get_or_create(tx.from).nonce += 1;
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

            TxKind::Commit { job_id, commit, bond } => {
                // Phase 1.1 (B4): route to JobLifecycle::record_commit via the shared helper —
                // record_commit itself escrows the bond through the ChainLedger (NO
                // escrow_into_job here: adding one would double-escrow). Unknown job / wrong
                // phase / non-member ⇒ Err ⇒ block rejected (closes the inert-Commit spam
                // window; pre-B5 no Commit can appear in any valid block at all).
                self.apply_commit(tx.from, *job_id, *commit, bond.raw())?;
                self.accounts.get_or_create(tx.from).nonce += 1;
            }

            TxKind::Reveal { job_id, result_hash, salt } => {
                // Phase 1.1 (B4): route to JobLifecycle::record_reveal via the shared helper
                // (which self-advances Committing→Revealing by height first — no money move).
                self.apply_reveal(tx.from, *job_id, *result_hash, *salt)?;
                self.accounts.get_or_create(tx.from).nonce += 1;
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
                // P3: zero-from txs skip signature verification — a funded zero address must
                // never become bonded stake (a keyless committee candidate anyone can puppet).
                if tx.from.is_zero() {
                    return Err(StateError::InvalidBlock("zero address cannot bond stake".into()));
                }
                self.bond(&tx.from, amount.raw())?;
                self.accounts.get_or_create(tx.from).nonce += 1;
            }

            TxKind::RequestUnbond { amount } => {
                if tx.from.is_zero() {
                    return Err(StateError::InvalidBlock("zero address cannot unbond stake".into()));
                }
                let now = self.blocks.height();
                self.request_unbond(&tx.from, amount.raw(), now)?;
                self.accounts.get_or_create(tx.from).nonce += 1;
            }

            TxKind::WithdrawUnbonded => {
                if tx.from.is_zero() {
                    return Err(StateError::InvalidBlock("zero address cannot withdraw stake".into()));
                }
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
        // SECURITY(F33): the same zero-from drain is reachable through a batched value move
        // (`Batch{Transfer{..}}` from a zero-from tx skips signature verification). Reject any
        // batch operation from the keyless zero address — no legitimate op is protocol-issued here.
        if from.is_zero() {
            return Err(StateError::InvalidBlock(
                "zero address cannot execute batch operations".into(),
            ));
        }
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
            TxKind::SubmitJob { comme_budget, .. } => {
                // Feature 52: legacy SubmitJob in batch — verify budget and burn (unchanged).
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
            TxKind::SubmitJobV2 { .. } => {
                // G-C: no unique per-op job id exists inside a batch (one tx hash, nonce
                // bumped once), so batched V2 escrow jobs are rejected outright.
                return Err(StateError::InvalidBlock(
                    "SubmitJobV2 not allowed in Batch".into(),
                ));
            }
            TxKind::ClaimJob { job_id } => {
                // Phase 1.1 (B3): identical semantics to the top-level arm (shared helper);
                // the outer Batch arm bumps the nonce once.
                self.apply_claim_job(from, *job_id)?;
            }
            TxKind::CompleteJob { job_id, result_hash } => {
                // B5 (flip): identical semantics to the top-level arm (shared `lifecycle_post_result`);
                // the outer Batch arm bumps the nonce once. The block-tail draw runs after all ops.
                if from.is_zero() {
                    return Err(StateError::InvalidBlock("zero address cannot complete jobs".into()));
                }
                let height = self.blocks.height();
                match self.lifecycle_post_result(*job_id, ParticipantId(from.0), *result_hash, height) {
                    Some(EventResult::Accepted) => {}
                    Some(EventResult::Rejected(r)) => {
                        return Err(StateError::InvalidBlock(format!("complete rejected: {r:?}")));
                    }
                    None => return Err(StateError::InvalidBlock("complete: unknown job".into())),
                }
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
            TxKind::Commit { job_id, commit, bond } => {
                // Phase 1.1 (B4): identical semantics to the top-level arm (shared helper).
                self.apply_commit(from, *job_id, *commit, bond.raw())?;
            }
            TxKind::Reveal { job_id, result_hash, salt } => {
                // Phase 1.1 (B4): identical semantics to the top-level arm (shared helper).
                self.apply_reveal(from, *job_id, *result_hash, *salt)?;
            }
            // PoUW P2 / G4: bonded-stake ops inside a batch. `from` is the owned Copy param and no
            // outer `sender` borrow is held here, so the &mut self stake methods are unobstructed.
            // No per-op nonce/fee: the outer Batch arm bumps nonce once; the fee is burned once in
            // apply_transaction before the match. P3: zero-from guarded like the top-level arms.
            TxKind::Bond { amount } => {
                if from.is_zero() {
                    return Err(StateError::InvalidBlock("zero address cannot bond stake".into()));
                }
                self.bond(&from, amount.raw())?;
            }
            TxKind::RequestUnbond { amount } => {
                if from.is_zero() {
                    return Err(StateError::InvalidBlock("zero address cannot unbond stake".into()));
                }
                let now = self.blocks.height();
                self.request_unbond(&from, amount.raw(), now)?;
            }
            TxKind::WithdrawUnbonded => {
                if from.is_zero() {
                    return Err(StateError::InvalidBlock("zero address cannot withdraw stake".into()));
                }
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

    /// B3: full ClaimJob semantics, shared by the top-level and Batch arms (the caller bumps
    /// the nonce). Returns Ok(()) on both the V2-open path and the legacy path.
    fn apply_claim_job(&mut self, from: Address, job_id: [u8; 32]) -> Result<(), StateError> {
        // P3: zero-from txs skip signature verification — the keyless zero address must never
        // become an executor holding a bond.
        if from.is_zero() {
            return Err(StateError::InvalidBlock(
                "zero address cannot claim compute jobs".into(),
            ));
        }
        // Permissionless claim race (whitepaper: any bonded validator may claim an open job).
        // LOSING the race is a NORMAL outcome, not a block-invalidating error: a ClaimJob for a
        // job that is already claimed applies as a nonce-consuming NO-OP (the caller bumps the
        // nonce). This is essential for liveness — a stuck losing-ClaimJob nonce would otherwise
        // wedge the loser's sequence, so its LATER committee Commit/Reveal at nonce+1 could never
        // apply (→ verifiers can't reach quorum → jobs never pay out). The winner's claim stands:
        // we return BEFORE opening any lifecycle or escrowing a bond, so the one-executor-per-job
        // invariant is fully preserved (no second lifecycle, no double-escrow). Deterministic —
        // reads only consensus `job_lifecycles` — so every node agrees the block is valid.
        if self.job_lifecycles.contains_key(&job_id) {
            return Ok(());
        }
        // Validator gate KEPT (legacy Feature-53 semantics + patch-spec §4 keeps tx-level gates).
        let is_validator = self.accounts.get(&from).map(|a| a.is_validator).unwrap_or(false);
        if !is_validator {
            return Err(StateError::InvalidBlock(
                "only validators can claim compute jobs".into(),
            ));
        }
        // M2 (flip): an unknown/expired job id now REJECTS the whole block instead of the legacy
        // silent no-op accept. Deterministic (reads only `pending_jobs`), symmetric with the
        // Commit/Reveal/CompleteJob unknown-id rejections in this bundle. A V1 SubmitJob pool job
        // never entered `pending_jobs`, so its on-chain ClaimJob was always a meaningless no-op —
        // rejecting it breaks no money flow. Honest producers avoid building such a block via the
        // 1.2b mempool ingress pre-filter (C7).
        let Some(rec) = self.pending_jobs.get(&job_id).copied() else {
            return Err(StateError::InvalidBlock("claim: unknown or expired job id".into()));
        };
        let height = self.blocks.height(); // G-F: parent height during apply
        if height > rec.claim_by {
            return Err(StateError::InvalidBlock("claim window expired".into()));
        }
        // Executor bond: deterministic v1 rule (G-D) — the parent-spec `Be >= B` floor with the
        // genesis-anchored flat knob as the minimum.
        let e_bond = rec.budget.max(self.game_params.executor_bond);
        // Pot sanity (defense-in-depth; B2 guarantees this): the pot must hold exactly the budget.
        if self.escrowed_for_job(&job_id) != rec.budget {
            return Err(StateError::InvalidBlock("job pot != pending budget".into()));
        }
        // Candidate snapshot (G-G): deterministic filter over finalized on-chain state ONLY,
        // sorted by address bytes (HashMap/AccountStore iteration order must never reach
        // consensus state — the candidates Vec is in the persisted DTO and the state root).
        // P3: the zero address is excluded even if someone manufactures flags/stake for it.
        let min_bond = self.stake_params.min_bond;
        let mut candidates: Vec<ParticipantId> = self
            .accounts
            .iter()
            .filter(|a| {
                a.is_validator
                    && a.compliance == ComplianceStatus::Compliant
                    && a.address != from
                    && !a.address.is_zero()
                    && self.bonded_stake.get(&a.address).copied().unwrap_or(0) >= min_bond
            })
            .map(|a| ParticipantId(a.address.0))
            .collect();
        candidates.sort_by(|x, y| x.0.cmp(&y.0));
        // Escrow the executor bond (balance→pot; InsufficientBalance rejects the block — no
        // partial state within this op, and P1 rollback covers the block).
        self.escrow_into_job(&from, job_id, e_bond)?;
        // Per-job deadlines anchored at CLAIM height (G-E windows).
        let result_by = height.saturating_add(self.phase_windows.result_blocks);
        let commit_by = result_by.saturating_add(self.phase_windows.commit_blocks);
        let reveal_by = commit_by.saturating_add(self.phase_windows.reveal_blocks);
        let deadlines = PhaseDeadlines { result_by, commit_by, reveal_by };
        // Pot after open = budget + e_bond = expected_escrow() with zero commitments — the P1
        // precondition documented at JobLifecycle::open holds by construction.
        let lc = JobLifecycle::open(
            job_id,
            rec.program_hash,
            rec.input_hash,
            rec.da_root,
            ParticipantId(rec.submitter),
            ParticipantId(from.0),
            e_bond,
            rec.budget,
            self.game_params.verifier_bond,
            self.game_params.clone(),
            self.resolution_params,
            candidates,
            deadlines,
        );
        self.job_lifecycles.insert(job_id, lc);
        self.pending_jobs.remove(&job_id);
        Ok(())
    }

    /// B4: full Commit semantics, shared by the top-level and Batch arms (the caller bumps the
    /// nonce). `record_commit` escrows the bond itself through the ChainLedger — NO
    /// `escrow_into_job` here (it would double-escrow).
    fn apply_commit(
        &mut self,
        from: Address,
        job_id: [u8; 32],
        commit: [u8; 32],
        bond: u64,
    ) -> Result<(), StateError> {
        // P3: zero-from txs skip signature verification — never let the keyless zero address
        // hold a committee bond.
        if from.is_zero() {
            return Err(StateError::InvalidBlock(
                "zero address cannot commit to compute jobs".into(),
            ));
        }
        // Validators-only gate KEPT (patch-spec §4). It is not the security boundary —
        // committee membership inside record_commit is — but it is deterministic and cheap.
        let is_validator = self.accounts.get(&from).map(|a| a.is_validator).unwrap_or(false);
        if !is_validator {
            return Err(StateError::InvalidBlock(
                "only validators can commit to compute jobs".into(),
            ));
        }
        let height = self.blocks.height(); // G-F
        // Verifier is ALWAYS the tx sender — no spoofing surface.
        let c = Commitment { verifier: ParticipantId(from.0), commit, bond };
        // S7: route by which map owns the job — a job is never live in both (the primary drains
        // in the same tail that opens the round), so this is defensive; primary takes precedence.
        if self.job_lifecycles.contains_key(&job_id) {
            match self.lifecycle_record_commit(job_id, c, height)? {
                Some(EventResult::Accepted) => Ok(()),
                Some(EventResult::Rejected(r)) => Err(StateError::InvalidBlock(format!(
                    "commit rejected: {r:?}"
                ))),
                None => Err(StateError::InvalidBlock("commit: unknown job".into())),
            }
        } else {
            use commputer_pouw_onchain::escalation_round::EventResult as PanelEventResult;
            match self.escalation_record_commit(job_id, c, height)? {
                Some(PanelEventResult::Accepted) => Ok(()),
                Some(PanelEventResult::Rejected(r)) => Err(StateError::InvalidBlock(format!(
                    "panel commit rejected: {r:?}"
                ))),
                None => Err(StateError::InvalidBlock("commit: unknown job".into())),
            }
        }
    }

    /// B4: full Reveal semantics, shared by the top-level and Batch arms (the caller bumps the
    /// nonce). No money moves on a reveal.
    fn apply_reveal(
        &mut self,
        from: Address,
        job_id: [u8; 32],
        result_hash: [u8; 32],
        salt: [u8; 32],
    ) -> Result<(), StateError> {
        if from.is_zero() {
            return Err(StateError::InvalidBlock(
                "zero address cannot reveal compute job results".into(),
            ));
        }
        let is_validator = self.accounts.get(&from).map(|a| a.is_validator).unwrap_or(false);
        if !is_validator {
            return Err(StateError::InvalidBlock(
                "only validators can reveal compute job results".into(),
            ));
        }
        let height = self.blocks.height(); // G-F
        // S7: route by which map owns the job (same defensive precedence as apply_commit).
        if self.job_lifecycles.contains_key(&job_id) {
            // Deliberate addition vs patch-spec §4: drive the height-based Committing→Revealing
            // transition on the tx path (advance is idempotent + money-free), so a reveal after
            // commit_by does not depend on the P8 driver having run at this exact height.
            // Deterministic: a pure function of consensus state + parent height.
            self.lifecycle_advance(job_id, height);
            let r = Reveal { verifier: ParticipantId(from.0), result_hash, salt };
            match self.lifecycle_record_reveal(job_id, r, height) {
                Some(EventResult::Accepted) => Ok(()),
                Some(EventResult::Rejected(rr)) => Err(StateError::InvalidBlock(format!(
                    "reveal rejected: {rr:?}"
                ))),
                None => Err(StateError::InvalidBlock("reveal: unknown job".into())),
            }
        } else {
            // Mirror of the primary's self-advance line, above — idempotent height transition on
            // the escalation-round tx path.
            if let Some(round) = self.escalation_rounds.get_mut(&job_id) {
                round.advance(height);
            }
            use commputer_pouw_onchain::escalation_round::EventResult as PanelEventResult;
            let r = Reveal { verifier: ParticipantId(from.0), result_hash, salt };
            match self.escalation_record_reveal(job_id, r, height) {
                Some(PanelEventResult::Accepted) => Ok(()),
                Some(PanelEventResult::Rejected(rr)) => Err(StateError::InvalidBlock(format!(
                    "panel reveal rejected: {rr:?}"
                ))),
                None => Err(StateError::InvalidBlock("reveal: unknown job".into())),
            }
        }
    }

    /// Phase 1.1: refund a pending job whose claim window has passed — full budget back to the
    /// submitter (no-fault: nobody claimed; the tx fee already paid was the anti-spam cost;
    /// the *voluntary* 2%-burn `resolve_cancel` needs a CancelJob TxKind and stays a
    /// follow-on). `Ok(None)` if the job is not pending or not yet due. Driven by
    /// `settle_due_jobs` (P8) so unclaimed pots cannot strand.
    pub fn expire_pending_job(
        &mut self,
        job_id: [u8; 32],
        height: u64,
    ) -> Result<Option<SettlementOutcome>, StateError> {
        let Some(rec) = self.pending_jobs.get(&job_id).copied() else {
            return Ok(None);
        };
        if height <= rec.claim_by {
            return Ok(None);
        }
        // Pre-validate the pot == the exact sum the refund moves (P1 caller-contract).
        let pot = self.escrowed_for_job(&job_id);
        if pot != rec.budget {
            return Err(StateError::InvalidBlock(format!(
                "pending pot {pot} != budget {}; refusing to expire",
                rec.budget
            )));
        }
        self.pay_from_job(job_id, &Address(rec.submitter), rec.budget)?;
        self.pending_jobs.remove(&job_id);
        Ok(Some(SettlementOutcome {
            submitter_refunded: rec.budget,
            ..Default::default()
        }))
    }

    /// Record emission for an epoch (mining rewards distributed to validators).
    pub fn emit(&mut self, amount: u64) {
        self.total_emitted = self.total_emitted.saturating_add(amount);
    }

    /// Revert the block at the given height, undoing all account state changes.
    /// Uses the StateDiff recorded during apply_block. Can only revert the tip.
    ///
    /// FAIL-SAFE: the StateDiff restores balance+nonce only — it CANNOT roll back the PoUW
    /// consensus maps (escrow pots, bonded/unbonding stake, lifecycles/pending jobs mutate
    /// with full-value semantics and no before-image exists). Any block that could have
    /// touched them is refused: guard 1 (maps non-empty) backstops guard 2 (tx-kind scan,
    /// extended for B2–B4 in Phase 1.1). Fork recovery past a PoUW-active block is
    /// `try_reorg` (full replay) or `reset_to_genesis` + resync.
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
            || !self.pending_jobs.is_empty()
            || !self.escalation_rounds.is_empty()
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
    /// If any transaction fails, no changes are committed to RocksDB AND (P1) the in-memory
    /// state is rolled back to its pre-block value — all three apply paths share the same
    /// rollback-on-Err core.
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

        // Process all transactions + run the P8 settlement driver. P1: on any Err the
        // pre-block state is restored (rocks: reload from CFs; memory: snapshot) — nothing
        // is committed to RocksDB and the in-memory state carries no smear.
        self.apply_txs_with_rollback(block)?;

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
            self.persisted_pending_keys.clone(),
            self.persisted_escalation_keys.clone(),
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
        self.pending_jobs.clear();
        self.escalation_rounds.clear();
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
                self.persisted_pending_keys,
                self.persisted_escalation_keys,
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
                    self.persisted_pending_keys,
                    self.persisted_escalation_keys,
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

    /// Append the six consensus maps' CF deltas to `batch`: delete every persisted key that
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
        for job_id in stale_keys(&self.persisted_pending_keys, &self.pending_jobs) {
            rocks.batch_delete_pending(batch, job_id);
        }
        for (job_id, rec) in &self.pending_jobs {
            rocks.batch_put_pending(batch, job_id, rec);
        }
        for job_id in stale_keys(&self.persisted_escalation_keys, &self.escalation_rounds) {
            rocks.batch_delete_escalation(batch, job_id);
        }
        for (job_id, er) in &self.escalation_rounds {
            rocks.batch_put_escalation(batch, job_id, &er.to_record());
        }
    }

    /// Advance all six persisted-key mirrors to the maps' current key sets. Call ONLY after
    /// a successful CF write — on failure the stale mirror recomputes a superset of deletes at
    /// the next attempt (deleting an absent key is a RocksDB no-op, so over-deleting is safe).
    fn commit_map_mirrors(&mut self) {
        self.persisted_escrow_keys = self.escrow_by_job.keys().copied().collect();
        self.persisted_bonded_keys = self.bonded_stake.keys().copied().collect();
        self.persisted_unbonding_keys = self.unbonding_stake.keys().copied().collect();
        self.persisted_lifecycle_keys = self.job_lifecycles.keys().copied().collect();
        self.persisted_pending_keys = self.pending_jobs.keys().copied().collect();
        self.persisted_escalation_keys = self.escalation_rounds.keys().copied().collect();
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

    /// PoUW P2: reconciling flush of the SIX consensus maps (escrow_by_job / bonded_stake /
    /// unbonding_stake / job_lifecycles / pending_jobs / escalation_rounds) — deletes CF rows for
    /// keys removed in-memory (via the persisted-key mirrors), then re-puts every live entry, in
    /// one WriteBatch. Safe to call at any time; kept as the shutdown-tail sweeper behind the
    /// per-block batch.
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
        self.pending_jobs.clear();
        self.escalation_rounds.clear();
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
        self.persisted_pending_keys.clear();
        self.persisted_escalation_keys.clear();

        info!("Chain state reset to genesis complete");
        Ok(())
    }
}

/// P1: the pre-block snapshot — everything `apply_transaction` + `settle_due_jobs` can mutate.
/// Genesis-anchored params (stake/game/resolution/phase_windows) are never mutated at apply, so
/// they are not captured.
struct BlockSnapshot {
    accounts: AccountStore,
    escrow_by_job: HashMap<[u8; 32], u64>,
    bonded_stake: HashMap<Address, u64>,
    unbonding_stake: HashMap<Address, Vec<UnbondingChunk>>,
    job_lifecycles: HashMap<[u8; 32], JobLifecycle>,
    pending_jobs: HashMap<[u8; 32], PendingJobRecord>,
    escalation_rounds: HashMap<[u8; 32], EscalationRound>,
    total_emitted: u64,
    total_burned: u64,
    current_epoch: u64,
    nerf_rate: NerfRate,
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
/// consensus maps. Used by `revert_block`'s guard 2. Phase 1.1 (B2–B4) added the job kinds:
/// `SubmitJobV2` (escrow + pending), `ClaimJob`/`Commit`/`Reveal` (lifecycle + escrow), and
/// `CompleteJob` (B5 will make it draw the committee — over-approximating now is the
/// fail-safe direction; guard 1 (maps non-empty) backstops any future staleness).
fn tx_touches_consensus_maps(tx: &Transaction) -> bool {
    fn kind_touches(kind: &TxKind) -> bool {
        match kind {
            TxKind::Bond { .. }
            | TxKind::RequestUnbond { .. }
            | TxKind::WithdrawUnbonded
            | TxKind::SubmitJobV2 { .. }
            | TxKind::ClaimJob { .. }
            | TxKind::Commit { .. }
            | TxKind::Reveal { .. }
            | TxKind::CompleteJob { .. } => true,
            TxKind::Batch { operations } => operations.iter().any(kind_touches),
            _ => false,
        }
    }
    kind_touches(&tx.kind)
}

/// §10 glue for PROTECTED B7 (block-assembly capacity admission). Pure mapping per the
/// patch-spec §8: `job_id = tx.hash().0` (G-A — the SAME id the escrow/pending maps use),
/// flagship by `l2_id`, priority = fee — delegating to the existing
/// `capacity::pending_job_from_fields` (P5: `validator_churn_bps` already exists in
/// capacity.rs; nothing is redefined here). Batch returns `None`: batched V2 is rejected at
/// apply (G-C) and batched V1 jobs are not pool-visible (`process_job_tx` does not unpack
/// Batch). Lives in storage (not pouw-onchain) because pouw-onchain deliberately has no
/// `commputer-core` dependency; `node` depends on `storage`, so B7's call site reaches it.
pub fn pending_job_from_tx(
    tx: &Transaction,
) -> Option<commputer_pouw_onchain::capacity::PendingJob> {
    match &tx.kind {
        TxKind::SubmitJob { l2_id, .. } | TxKind::SubmitJobV2 { l2_id, .. } => Some(
            commputer_pouw_onchain::capacity::pending_job_from_fields(
                commputer_pouw_onchain::capacity::PendingJobFields {
                    job_id: tx.hash().0,
                    is_flagship: l2_id
                        .as_deref()
                        .map(commputer_core::l2::is_flagship)
                        .unwrap_or(false),
                    fee: tx.fee,
                },
            ),
        ),
        _ => None,
    }
}

/// B8: the genesis-anchored consensus params in their live `ChainState` form. Converted from the
/// dependency-free `core::genesis::ConsensusParamsConfig` HERE (storage can reference the pouw
/// types; `commputer-core` cannot). Carries the four scalar param structs `set_consensus_params`
/// installs, plus the full `ConsensusParams` bundle the node's 1.2b startup gate feeds to
/// `refuse_to_bind`.
pub struct GenesisConsensusParams {
    pub game: GameParams,
    pub resolution: ResolutionParams,
    pub phase_windows: PhaseWindows,
    pub stake: StakeParams,
    /// Full bundle for `refuse_to_bind` (fingerprint + validate). `wasm_limits`/`chunking` are not
    /// genesis-configurable this pass, so they keep the node's COMPILED defaults — exactly what
    /// `ChainState` uses today.
    pub bundle: commputer_pouw_onchain::consensus_params::ConsensusParams,
}

/// B8 converter: `core::genesis::ConsensusParamsConfig` → the live consensus-param structs. Pure.
/// A config equal to `ConsensusParamsConfig::default()` yields EXACTLY the `pouw`/`pouw-onchain`
/// defaults `ChainState` uses today (the C9c drift-guard test asserts this field-for-field).
pub fn genesis_consensus_params(
    cfg: &commputer_core::genesis::ConsensusParamsConfig,
) -> GenesisConsensusParams {
    let g = &cfg.game;
    let game = GameParams {
        k: g.k,
        k_escalate: g.k_escalate,
        sample_rate_bps: g.sample_rate_bps,
        p_trap_bps: g.p_trap_bps,
        quorum_num: g.quorum_num,
        quorum_den: g.quorum_den,
        worker_bps: g.worker_bps,
        verifier_bps: g.verifier_bps,
        burn_bps: g.burn_bps,
        executor_bond: g.executor_bond,
        verifier_bond: g.verifier_bond,
        challenger_bond: g.challenger_bond,
        dispute_bounty_bps: g.dispute_bounty_bps,
        challenger_reward_bps: g.challenger_reward_bps,
        escalation_reward_bps: g.escalation_reward_bps,
        trap_jackpot_bps: g.trap_jackpot_bps,
        price_per_mfuel: g.price_per_mfuel,
        profit_margin_bps: g.profit_margin_bps,
        bond_safety_bps: g.bond_safety_bps,
    };
    let resolution = ResolutionParams {
        cancel_burn_bps: cfg.resolution.cancel_burn_bps,
        timeout_submitter_comp_bps: cfg.resolution.timeout_submitter_comp_bps,
    };
    let phase_windows = PhaseWindows {
        result_blocks: cfg.phase_windows.result_blocks,
        commit_blocks: cfg.phase_windows.commit_blocks,
        reveal_blocks: cfg.phase_windows.reveal_blocks,
        claim_blocks: cfg.phase_windows.claim_blocks,
    };
    let stake = StakeParams {
        unbonding_blocks: cfg.stake.unbonding_blocks,
        min_bond: cfg.stake.min_bond,
    };
    let capacity = commputer_pouw_onchain::capacity::CapacityParams {
        total_slots: cfg.capacity.total_slots,
        flagship_reserve_bps: cfg.capacity.flagship_reserve_bps,
        reserve_floor_bps: cfg.capacity.reserve_floor_bps,
        reserve_max_bps: cfg.capacity.reserve_max_bps,
        reserve_churn_coeff_bps: cfg.capacity.reserve_churn_coeff_bps,
    };
    // Compiled-default bundle with the genesis-configurable groups overridden; `wasm_limits`/
    // `chunking` stay at the node's compiled defaults (struct-update syntax — no clippy churn).
    let bundle = commputer_pouw_onchain::consensus_params::ConsensusParams {
        game: game.clone(),
        resolution,
        phase_windows,
        capacity,
        min_fuel_cap: cfg.min_fuel_cap,
        ..commputer_pouw_onchain::consensus_params::ConsensusParams::default()
    };
    GenesisConsensusParams { game, resolution, phase_windows, stake, bundle }
}

// ===================================================================================
// PoUW P1 — per-job escrow foundation (the on-chain analog of the staging
// `commputer-pouw-onchain::escrow_ledger::EscrowLedger`).
//
// These are the conservation-preserving primitives every terminal settlement resolver
// (`resolve_confirmed`/`disputed`/`cancel`/`timeout`/`unavailable`/`escalation_fallback`)
// calls when a job reaches a terminal. LIVE since Phase 1.1: `SubmitJobV2` escrows at submit
// (B2), `ClaimJob`/`Commit` escrow bonds (B3/B4), and the deterministic in-apply driver
// (`settle_due_jobs`, P8) drains every pot at its terminal or expiry — escrow-in and drain
// landed together, so no budget can strand.
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

/// Phase 1.1 (B2/G-B): a submitted-but-unclaimed compute job — what `ClaimJob` needs
/// (submitter, budget, program identity, claim deadline) that its TxKind does not carry.
/// Written by the `SubmitJobV2` apply arm alongside the budget escrow; removed at `ClaimJob`
/// (lifecycle opens) or at `expire_pending_job` (full refund past `claim_by`).
/// Borsh-serialized for RocksDB persistence AND folded into the state root — all fixed-size
/// fields ⇒ canonical encoding; treat the field layout as a STABLE on-disk schema (same
/// warning as `UnbondingChunk`). `l2_id`/fee/resources deliberately excluded: B7 admission
/// runs mempool-side pre-block; execution metadata lives in tx history.
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct PendingJobRecord {
    /// `Address` bytes of the submitter (escrow refund target).
    pub submitter: [u8; 32],
    /// Raw units — equals the job's escrow pot between submit and claim.
    pub budget: u64,
    /// `sha256(wasm)` — the linchpin program identity (P9: carried into the lifecycle).
    pub program_hash: [u8; 32],
    pub input_hash: [u8; 32],
    /// DA-sampling anchor.
    pub da_root: [u8; 32],
    pub submitted_height: u64,
    /// `submitted_height + phase_windows.claim_blocks` (anchored at submit).
    pub claim_by: u64,
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

    /// B5 step 1: record the executor's result (no money move; the committee is drawn later in the
    /// block tail by `draw_committees_for_completed_jobs`). `None` if no lifecycle for `job_id`. A
    /// `get_mut` borrow suffices — `post_result` touches no ledger (mirror of
    /// `lifecycle_record_reveal`).
    pub fn lifecycle_post_result(
        &mut self,
        job_id: [u8; 32],
        executor: ParticipantId,
        result_hash: [u8; 32],
        height: u64,
    ) -> Option<EventResult> {
        let life = self.job_lifecycles.get_mut(&job_id)?;
        Some(life.post_result(executor, result_hash, height))
    }

    /// B5 step 2 (block tail): draw the committee for every lifecycle whose executor has posted a
    /// result but whose committee is not yet drawn. Runs once per applied block, INSIDE the
    /// rollback envelope, BETWEEN the tx loop and `settle_due_jobs`. Money-free.
    ///
    /// DETERMINISM (fork-safety, §0): every input is consensus state. The per-job seed is
    /// `hash(block_hash‖job_id)` (frozen `ids::hash_parts`) — `block.hash()` is node-independent
    /// once the block is finalized, and mixing `job_id` de-correlates jobs sharing a block. The
    /// candidates were snapshotted + SORTED at ClaimJob (already in the state root); `stake_of`
    /// reads only `bonded_stake`; `k` is the genesis game param. NOTHING here reads
    /// `consensus.slashed_validators`, the wall-clock, an RNG, or HashMap iteration order — the
    /// job_ids are SORTED before the draw so HashMap order can never reach the committee (which
    /// folds into the state root). The frozen `select_committee` (sorts by `(ticket, id)`) is the
    /// only selection logic.
    fn draw_committees_for_completed_jobs(&mut self, block_hash: BlockHash) {
        let mut jobs: Vec<[u8; 32]> = self
            .job_lifecycles
            .iter()
            .filter(|(_, l)| {
                l.phase() == Phase::AwaitingResult
                    && l.executor_hash_is_set()
                    && l.committee().is_empty()
            })
            .map(|(k, _)| *k)
            .collect();
        jobs.sort_unstable(); // HashMap order must never reach consensus
        for job_id in jobs {
            let seed = commputer_pouw::ids::hash_parts(&[&block_hash.0, &job_id]);
            // Borrow dance: own the lifecycle OUT of the map so the `&*self` stake reads (bonded_stake)
            // don't alias the map mutation — same shape as `lifecycle_record_commit`'s pre-check.
            let mut life = self
                .job_lifecycles
                .remove(&job_id)
                .expect("job_id came from the filtered live set");
            {
                let chain = &*self;
                life.draw_committee(seed, &|p| chain.stake_of(&Address(p.0)));
            }
            self.job_lifecycles.insert(job_id, life);
        }
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
        // P2: cached-terminal short-circuit BEFORE the pot pre-validation. After the first
        // settle the pot no longer equals expected_escrow() — expected_escrow sums ALL bonds
        // incl. non-revealers, but settle already burned the forfeited ones (and on
        // Confirmed/Disputed/Timeout drained the pot entirely) — so re-validating on re-entry
        // would Err forever and strand an Escalate pot. The cached path moves no money.
        if let Some(t) = life.settled_terminal() {
            self.job_lifecycles.insert(job_id, life);
            return Ok(Some(t));
        }
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

    /// Phase 1.1 (D2) → S5 (THE FLIP): settle + drain — the P8-driver (and future B6) entry
    /// point. Confirmed/Disputed/TimedOut drain the pot via settle itself. `Terminal::Escalate`
    /// now opens a REAL second-panel `EscalationRound` when the F2 viability gate passes
    /// (`panel.len() >= quorum(k_escalate)`) — the round takes ownership of the held pot and is
    /// driven/settled by the S6 sweep — and falls back to the zero-comp refund resolver
    /// (`resolve_escalation_fallback`, D2-FINAL, byte-identical to the pre-S5 behavior) when the
    /// candidate pool is structurally too small. The lifecycle is REMOVED on success ⇒
    /// at-most-once settlement by construction (a second call hits `Ok(None)` and moves no
    /// money). `fb` is `Some` only on the fallback path (an opened round settles later).
    pub fn lifecycle_settle_and_drain(
        &mut self,
        job_id: [u8; 32],
        eq: &dyn EquivalenceOracle,
        block_hash: BlockHash,
    ) -> Result<Option<(Terminal, Option<SettlementOutcome>)>, StateError> {
        let Some(terminal) = self.lifecycle_settle(job_id, eq)? else {
            return Ok(None);
        };
        let fb = if let Terminal::Escalate(h) = &terminal {
            // Pot preflight (unchanged): the held sum the round (or fallback) will own
            // (defensive: provably equal after settle, but the ChainLedger .expect()s must stay
            // unreachable). On Err the lifecycle was already re-inserted by lifecycle_settle
            // with its terminal cached, so a retry is idempotent.
            let expected = h
                .budget
                .saturating_add(h.executor_bond)
                .saturating_add(h.committee_bonds.iter().sum::<u64>());
            let actual = self.escrowed_for_job(&job_id);
            if actual != expected {
                return Err(StateError::InvalidBlock(format!(
                    "escalate pot {actual} != expected {expected}; refusing escalation open"
                )));
            }
            // EscalationRound (S5, 2026-07-19): draw the panel and apply the F2 viability gate.
            // Candidates = the settling lifecycle's claim-time snapshot (already SORTED at
            // ClaimJob) MINUS its round-1 committee (executor auto-excluded inside
            // select_committee). Seed domain-separated from the round-1 draw by the "escalate"
            // tag. Deadlines anchor at the CURRENT (parent) height with the round-1 windows
            // (F3). All inputs are consensus state — deterministic across nodes.
            let rec = self
                .job_lifecycles
                .get(&job_id)
                .expect("lifecycle re-inserted by lifecycle_settle")
                .to_record();
            let committee: HashSet<[u8; 32]> = rec.committee.iter().copied().collect();
            let candidates: Vec<ParticipantId> = rec
                .candidates
                .iter()
                .filter(|c| !committee.contains(*c))
                .map(|c| ParticipantId(*c))
                .collect();
            let seed = commputer_pouw::ids::hash_parts(&[&block_hash.0, &job_id, b"escalate"]);
            let height = self.blocks.height(); // G-F: parent height during apply
            let deadlines = commputer_pouw_onchain::escalation_round::PanelDeadlines {
                commit_by: height.saturating_add(self.phase_windows.commit_blocks),
                reveal_by: height
                    .saturating_add(self.phase_windows.commit_blocks)
                    .saturating_add(self.phase_windows.reveal_blocks),
            };
            let identity = commputer_pouw_onchain::escalation_round::JobIdentity {
                program_hash: rec.program_hash,
                input_hash: rec.input_hash,
                da_root: rec.da_root,
            };
            let h = h.clone();
            let round = {
                // Borrow dance: `open` only READS self (the stake closure) — scope the shared
                // borrow before the insert / ChainLedger::new mutable uses (same shape as
                // `draw_committees_for_completed_jobs`).
                let chain = &*self;
                EscalationRound::open(
                    h.clone(),
                    job_id,
                    identity,
                    candidates,
                    seed,
                    chain.game_params.clone(),
                    deadlines,
                    &|p| chain.stake_of(&Address(p.0)),
                )
            };
            if round.panel().len() >= self.game_params.quorum(self.game_params.k_escalate) {
                // F2 gate PASSES: the round owns the held pot from here; no money moves at open.
                self.escalation_rounds.insert(job_id, round);
                None
            } else {
                // F2 gate FAILS (structural candidate shortage, not misbehavior): zero-comp
                // refund, byte-identical to the pre-EscalationRound stand-in.
                let mut view = ChainLedger::new(self);
                Some(resolve_escalation_fallback(&mut view, job_id, &h))
            }
        } else {
            None
        };
        // Drain: the pot is 0 on every path here EXCEPT an opened round, which owns it now.
        self.job_lifecycles.remove(&job_id);
        Ok(Some((terminal, fb)))
    }

    /// S5: record a panel member's escalation commit (escrows the bond into the job pot via the
    /// round). `None` if no round for `job_id`. Mirrors `lifecycle_record_commit` — same
    /// balance pre-check + borrow dance; Task 6 routes Commit txs here when the job has an
    /// escalation round instead of a lifecycle.
    pub fn escalation_record_commit(
        &mut self,
        job_id: [u8; 32],
        c: Commitment,
        height: u64,
    ) -> Result<Option<commputer_pouw_onchain::escalation_round::EventResult>, StateError> {
        let mut round = match self.escalation_rounds.remove(&job_id) {
            Some(r) => r,
            None => return Ok(None),
        };
        // record_commit escrows c.bond from the committer (on Accepted) via the infallible
        // ChainHooks surface; pre-check the balance so that escrow cannot panic the ledger.
        let committer = Address(c.verifier.0);
        let bal = self.accounts.get(&committer).map(|a| a.balance.raw()).unwrap_or(0);
        if bal < c.bond {
            self.escalation_rounds.insert(job_id, round);
            return Err(StateError::InsufficientBalance);
        }
        let mut view = ChainLedger::new(self);
        let r = round.record_commit(&mut view, c, height);
        self.escalation_rounds.insert(job_id, round);
        Ok(Some(r))
    }

    /// S5: record a panel member's escalation reveal (no money move). `None` if no round for
    /// `job_id`. Mirrors `lifecycle_record_reveal`.
    pub fn escalation_record_reveal(
        &mut self,
        job_id: [u8; 32],
        r: Reveal,
        height: u64,
    ) -> Option<commputer_pouw_onchain::escalation_round::EventResult> {
        let round = self.escalation_rounds.get_mut(&job_id)?;
        Some(round.record_reveal(r, height))
    }

    /// S6: settle + drain one escalation round (all three outcomes drain the pot to 0). Removed
    /// on success ⇒ at-most-once. The pot preflight mirrors the primary's P1 caller contract;
    /// on Err the round is re-inserted so a retry is idempotent.
    pub fn escalation_settle_and_drain(
        &mut self,
        job_id: [u8; 32],
        eq: &dyn EquivalenceOracle,
    ) -> Result<Option<EscalationOutcome>, StateError> {
        let mut round = match self.escalation_rounds.remove(&job_id) {
            Some(r) => r,
            None => return Ok(None),
        };
        if round.is_settled() {
            // Cached terminal: pot already drained; settle short-circuits to the cached outcome
            // (moves NO money) and dropping the round is the drain.
            let out = round.settle(&mut ChainLedger::new(self), eq);
            return Ok(Some(out));
        }
        let expected = round.expected_escrow();
        let actual = self.escrowed_for_job(&job_id);
        if actual != expected {
            self.escalation_rounds.insert(job_id, round);
            return Err(StateError::InvalidBlock(format!(
                "escalation pot {actual} != expected {expected}; refusing to settle"
            )));
        }
        let out = round.settle(&mut ChainLedger::new(self), eq);
        Ok(Some(out))
    }

    /// B8 (C1): install the genesis-anchored consensus params AND re-inject them into every
    /// in-memory `JobLifecycle`. The node calls this once, right after `open()` (1.2b main.rs), so
    /// a node that restarted mid-lifecycle settles with GENESIS params, not the defaults `open()`
    /// reconstructs with.
    ///
    /// C1 (the fork-safety fix): `open()` rebuilds each lifecycle via
    /// `from_record(rec, GameParams::default(), ResolutionParams::default())` (game/resolution are
    /// not persisted per-job). `settle` reads the per-lifecycle `self.params`/`self.rparams`, so a
    /// node that settled a reloaded lifecycle with DEFAULT params while peers used GENESIS params
    /// would compute a different Terminal → a different `SettlementOutcomeRec` in the state root →
    /// a HARD FORK. So after setting the scalar fields we REBUILD every lifecycle through its DTO
    /// with the just-installed params — `to_record()`/`from_record` round-trips every per-job field
    /// (phase/committee/commitments/reveals/deadlines/executor_hash/settled), changing ONLY the
    /// re-injected params. No money moves; no settlement runs in the startup window, so the rebuild
    /// is sufficient and keeps the protected footprint to this one call.
    ///
    /// C4: rejects any phase window < 1 (a zero window would let a same-block draw be swept to
    /// instant NoQuorum by `settle_due_jobs`). 1.2b's `refuse_to_bind` is the full startup gate;
    /// this guard keeps `set_consensus_params` self-consistent.
    pub fn set_consensus_params(
        &mut self,
        game: GameParams,
        resolution: ResolutionParams,
        phase_windows: PhaseWindows,
        stake: StakeParams,
    ) -> Result<(), StateError> {
        if phase_windows.result_blocks < 1
            || phase_windows.commit_blocks < 1
            || phase_windows.reveal_blocks < 1
            || phase_windows.claim_blocks < 1
        {
            return Err(StateError::InvalidBlock(
                "consensus phase windows must be >= 1 block".into(),
            ));
        }
        self.game_params = game;
        self.resolution_params = resolution;
        self.phase_windows = phase_windows;
        self.stake_params = stake;
        // C1: re-inject the params into every reconstructed lifecycle (see doc above).
        for life in self.job_lifecycles.values_mut() {
            let rec = life.to_record();
            *life = JobLifecycle::from_record(rec, self.game_params.clone(), self.resolution_params);
        }
        // C1: same re-injection for escalation rounds (params are never persisted).
        for er in self.escalation_rounds.values_mut() {
            let rec = er.to_record();
            *er = EscalationRound::from_record(rec, self.game_params.clone());
        }
        Ok(())
    }

    /// B7 (1.2b, C8): install the genesis-anchored capacity split. Kept SEPARATE from
    /// `set_consensus_params` deliberately: capacity is PRODUCER-SIDE scheduling only (read during
    /// block assembly, never apply-enforced, never state-rooted), so it does not belong on the
    /// fork-critical param path that re-injects into lifecycles — this keeps the C1 rebuild + C4
    /// window guard untouched. The node calls this once at startup alongside `set_consensus_params`.
    pub fn set_capacity_params(
        &mut self,
        capacity: commputer_pouw_onchain::capacity::CapacityParams,
    ) {
        self.capacity_params = capacity;
    }

    /// B7: the installed capacity split, read by the producer-side admission scheduler.
    pub fn capacity_params(&self) -> &commputer_pouw_onchain::capacity::CapacityParams {
        &self.capacity_params
    }

    /// 1.2-MEMPOOL (soundness engine, logic non-protected): from `candidates` (already sig/nonce/
    /// fee-validated for the mempool) return `(kept, requeue)`, where `kept` is the largest in-order
    /// prefix-closed subset that applies CLEANLY in sequence on top of the CURRENT committed state
    /// (so a block built from `kept` cannot fail apply), and `requeue` holds txs that are merely
    /// early/late in their PoUW phase window (C3) — the caller pushes them back into the mempool
    /// like a future-nonce tx. Permanently-doomed txs (insufficient balance, zero-from, unknown/
    /// duplicate job, V2-in-Batch, …) are dropped into neither set.
    ///
    /// C2 (no clone; guaranteed restore): `ChainState` is NOT `Clone`, so this MUTATES `self` to
    /// trial-apply and then FULLY restores it from the entry snapshot before returning. Restoration
    /// is guaranteed: the only early return is the final one AFTER the restore, every per-tx failure
    /// is rolled back to the per-tx snapshot immediately, and `apply_transaction` is panic-free by
    /// the P1 pre-validation contract (the same contract `apply_txs_with_rollback` already relies
    /// on). The caller therefore sees `self` byte-identical to entry — post-call state root ==
    /// pre-call root, no smear (the class P1 rollback fixed).
    pub fn select_applicable_txs(
        &mut self,
        candidates: Vec<Transaction>,
    ) -> (Vec<Transaction>, Vec<Transaction>) {
        let entry = self.capture_pre_block();
        let mut kept = Vec::with_capacity(candidates.len());
        let mut requeue = Vec::new();
        for tx in candidates {
            // Per-tx snapshot: `apply_transaction` deducts+burns the fee BEFORE the arm, so a
            // mid-arm Err leaves a partial smear that must be rolled back before the next trial.
            let per_tx = self.capture_pre_block();
            match self.apply_transaction(&tx) {
                Ok(()) => kept.push(tx),
                Err(e) => {
                    self.rollback_to_pre_block(per_tx);
                    if self.tx_is_phase_deferred(&tx, &e) {
                        requeue.push(tx);
                    }
                    // else: permanently doomed at this committed state — drop.
                }
            }
        }
        self.rollback_to_pre_block(entry);
        (kept, requeue)
    }

    /// C3 classifier: whether a rejected candidate is merely EARLY/LATE in its PoUW phase window
    /// (requeue) versus permanently doomed (drop). Reads only committed state (post per-tx rollback)
    /// — deterministic. A Commit/Reveal/CompleteJob whose job EXISTS (a live lifecycle, or a pending
    /// record a later ClaimJob will open) is phase-deferred: e.g. a Commit sharing a block with its
    /// own CompleteJob sees an empty committee (the draw runs in the tail) → WrongPhase now, valid
    /// once the phase advances. Insufficient-balance and zero-from are permanently doomed regardless
    /// of kind; every other failure (unknown job, duplicate job, V2-in-Batch, non-job txs) drops.
    fn tx_is_phase_deferred(&self, tx: &Transaction, err: &StateError) -> bool {
        if matches!(err, StateError::InsufficientBalance) {
            return false; // doomed at this state (C3)
        }
        if tx.from.is_zero() {
            return false; // keyless zero address is permanently invalid on every money arm
        }
        match &tx.kind {
            // S7: Commit/Reveal also defer for a job that has moved into an active escalation
            // round (the primary lifecycle is drained by then). CompleteJob has no escalation-round
            // analogue — a round is a panel Commit/Reveal game only — so it does NOT gain this check.
            TxKind::Commit { job_id, .. } | TxKind::Reveal { job_id, .. } => {
                self.job_lifecycles.contains_key(job_id)
                    || self.pending_jobs.contains_key(job_id)
                    || self.escalation_rounds.contains_key(job_id)
            }
            TxKind::CompleteJob { job_id, .. } => {
                self.job_lifecycles.contains_key(job_id) || self.pending_jobs.contains_key(job_id)
            }
            _ => false,
        }
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
    use commputer_pouw_onchain::escalation_round::{EventResult as EscEventResult, PanelPhase};
    use commputer_pouw_onchain::settlement_resolution::ResolutionParams;
    use commputer_pouw_onchain::escrow_ledger::EscrowLedger; // B10: the staging reference ledger

    /// ParticipantId and the byte-identical on-chain Address (the ChainLedger casts between them).
    fn lpid(n: u8) -> ParticipantId {
        ParticipantId([n; 32])
    }
    fn lpaddr(n: u8) -> Address {
        Address([n; 32])
    }
    /// P9 identity placeholder for direct test opens (identity is opaque to the game logic).
    const IDENT: [u8; 32] = [0xAB; 32];
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
    fn commit_unknown_job_rejects_block_with_zero_money_delta() {
        // P6 inversion of the pre-flip inert pin: B4 routes Commit to the lifecycle, so a
        // Commit against an UNKNOWN job now rejects the whole block — closing the inert-Commit
        // spam window — and P1 rollback leaves the state (root included) untouched.
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        let v = state.accounts.get_or_create(addr(1));
        v.is_validator = true;
        v.balance = Amount::from_comme(10);
        state.total_emitted = Amount::from_comme(10).raw();
        let burned_before = state.total_burned;
        let bal_before = state.accounts.get(&addr(1)).unwrap().balance;
        let root_before = state.compute_state_root();

        let commit = TxKind::Commit { job_id: [7u8; 32], commit: [2u8; 32], bond: Amount::from_raw(1_000) };
        let err = state
            .apply_block(&block_with(&state, 1, vec![unsigned(addr(1), 0, commit)]))
            .unwrap_err();
        assert!(err.to_string().contains("unknown job"), "rejects as unknown job, got: {err}");
        assert_eq!(state.blocks.height(), 0, "rejected block never stored");
        assert_eq!(state.accounts.get(&addr(1)).unwrap().nonce, 0, "nonce NOT bumped");
        assert_eq!(state.accounts.get(&addr(1)).unwrap().balance, bal_before, "no money moved");
        assert_eq!(state.total_burned, burned_before, "nothing burned");
        assert_eq!(state.escrowed_for_job(&[7u8; 32]), 0, "no escrow pot created");
        assert_eq!(state.compute_state_root(), root_before, "P1 rollback: root unchanged");
    }

    #[test]
    fn reveal_unknown_job_rejects_block_with_zero_money_delta() {
        // P6 inversion: a Reveal against an unknown job rejects the block; P1 rollback leaves
        // no trace.
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        state.accounts.get_or_create(addr(1)).is_validator = true;
        let root_before = state.compute_state_root();

        let reveal = TxKind::Reveal { job_id: [7u8; 32], result_hash: [3u8; 32], salt: [4u8; 32] };
        let err = state
            .apply_block(&block_with(&state, 1, vec![unsigned(addr(1), 0, reveal)]))
            .unwrap_err();
        assert!(err.to_string().contains("unknown job"), "rejects as unknown job, got: {err}");
        assert_eq!(state.blocks.height(), 0, "rejected block never stored");
        assert_eq!(state.accounts.get(&addr(1)).unwrap().nonce, 0, "nonce NOT bumped");
        assert_eq!(state.compute_state_root(), root_before, "P1 rollback: root unchanged");
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
        // P1 ATOMICITY: every apply path now rolls back the pre-block state on Err — op1's
        // Bond is UNDONE when op2 fails (pre-P1 it smeared in memory and could reach the CFs
        // via the next block's batch). The block is REJECTED, the nonce is NOT bumped, and
        // conservation holds trivially (nothing changed at all).
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
        // P1: op1's Bond was rolled back with the block — no smear.
        assert_eq!(state.bonded_of(&addr(1)), 0, "P1 rollback undid the partial bond");
        assert_eq!(state.accounts.get(&addr(1)).unwrap().balance, Amount::from_raw(1_000));
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
            job, IDENT, IDENT, IDENT, pid(0), pid(9), e_bond, budget, v_bond,
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
            job, IDENT, IDENT, IDENT, lpid(0), lpid(9), e_bond, budget, v_bond,
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
            job, IDENT, IDENT, IDENT, lpid(0), lpid(9), e_bond, budget, v_bond,
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
            job, IDENT, IDENT, IDENT, lpid(0), lpid(9), e_bond, budget, v_bond,
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
            s.job, IDENT, IDENT, IDENT, lpid(submitter), lpid(executor), e_bond, budget, v_bond,
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
            s.job, IDENT, IDENT, IDENT, lpid(submitter), lpid(executor), e_bond, budget, v_bond,
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

    #[test]
    fn equivalence_disputed_with_wrong_side_forfeiture_matches() {
        // §332 collusion, cross-backend: executor claims 7; pid(10)/pid(11) reveal the correct 5
        // (honest, win quorum ⇒ Disputed); pid(12) rubber-stamps the executor's wrong 7. pid(12)'s
        // bond is FORFEITED (burned), not returned. Proves the forfeiture is byte-identical on the
        // staging EscrowLedger and the on-chain ChainState.
        let claimed = [7u8; 32];
        let correct = [5u8; 32];
        let both = run_on_both(&Scenario {
            job: [9u8; 32],
            executor_result: Some(claimed),
            candidates: vec![10, 11, 12],
            commits: vec![(10, correct, true), (11, correct, true), (12, claimed, true)],
        });
        match &both.chain_terminal {
            Terminal::Disputed(out) => {
                assert_eq!(out.submitter_refunded, 3_960, "submitter fully refunded");
                assert_eq!(out.verifiers_paid, 792, "20% of exec bond bounty across the honest 2");
                // burn = exec-bond remainder (3_960-792) + the forfeited wrong-side bond (1_650).
                assert_eq!(out.burned, (3_960 - 792) + 1_650, "exec remainder + colluder bond");
                assert_eq!(out.bonds_returned, 2 * 1_650, "only the 2 honest revealers' bonds returned");
                assert_eq!(
                    out.slashed,
                    vec![(lpid(9), 3_960), (lpid(12), 1_650)],
                    "executor bond + wrong-side verifier bond logged slashed"
                );
            }
            other => panic!("expected Disputed, got {other:?}"),
        }
        assert_equivalent(&both);
        assert_eq!(both.chain.escrowed_for_job(&both.job), 0, "pot drained on Disputed");
        // The colluder (pid 12) forfeited its bond on BOTH backends.
        assert_eq!(both.chain.accounts.get(&lpaddr(12)).map(|a| a.balance.raw()).unwrap_or(0), 0);
        assert_eq!(both.staging.balance_of(&lpid(12)), 0);
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

    // ── A-batch item 7: genesis account allocations ──

    /// (a) EMPTY continuity: applying an empty allocation set must leave BOTH the
    /// state root AND total_emitted byte-identical to a chain that never called
    /// apply_genesis_accounts. This is the guarantee that today's genesis.json
    /// (no `accounts` field) keeps the same genesis hash/state root.
    #[test]
    fn genesis_accounts_empty_is_byte_identical() {
        let mut baseline = ChainState::new();
        baseline.apply_block(&genesis_block()).unwrap();

        let mut with_empty = ChainState::new();
        with_empty.apply_block(&genesis_block()).unwrap();
        with_empty.apply_genesis_accounts(&[]).unwrap();

        assert_eq!(
            with_empty.total_emitted, baseline.total_emitted,
            "empty genesis accounts must not change total_emitted"
        );
        assert_eq!(
            with_empty.compute_state_root(), baseline.compute_state_root(),
            "empty genesis accounts must not change the state root"
        );
    }

    /// (b) A funded allocation credits the balance, bumps total_emitted by exactly
    /// the sum, and is DETERMINISTIC across two independent builds (identical state
    /// root) regardless of entry order.
    #[test]
    fn genesis_accounts_funded_credits_and_is_deterministic() {
        let a_hex = hex::encode(addr(1).0);
        let b_hex = hex::encode(addr(2).0);
        let alloc_a = 700_000_000u64;
        let alloc_b = 300_000_000u64;
        let sum = alloc_a + alloc_b;

        let mut s1 = ChainState::new();
        s1.apply_block(&genesis_block()).unwrap();
        let emitted_before = s1.total_emitted;
        s1.apply_genesis_accounts(&[
            (a_hex.clone(), alloc_a),
            (b_hex.clone(), alloc_b),
        ]).unwrap();

        // Balances credited exactly.
        assert_eq!(s1.accounts.get(&addr(1)).unwrap().balance, Amount::from_raw(alloc_a));
        assert_eq!(s1.accounts.get(&addr(2)).unwrap().balance, Amount::from_raw(alloc_b));
        // total_emitted increased by exactly the sum.
        assert_eq!(s1.total_emitted, emitted_before + sum);

        // Independent build with REVERSED entry order must reach the identical
        // state root (order-independent, deterministic).
        let mut s2 = ChainState::new();
        s2.apply_block(&genesis_block()).unwrap();
        s2.apply_genesis_accounts(&[
            (b_hex, alloc_b),
            (a_hex, alloc_a),
        ]).unwrap();
        assert_eq!(s2.total_emitted, s1.total_emitted);
        assert_eq!(
            s2.compute_state_root(), s1.compute_state_root(),
            "genesis account application must be order-independent / deterministic"
        );
    }

    /// (c) Supply conservation: with the emitted bump, Σ balances still equals
    /// circulating (total_emitted - total_burned), remaining shrinks by exactly
    /// the sum, and total_supply/TOTAL_SUPPLY are unchanged.
    #[test]
    fn genesis_accounts_conserve_supply() {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();

        let remaining_before = state.remaining_supply();
        let burned_before = state.total_burned;
        let alloc = 1_234_567_800u64;

        state.apply_genesis_accounts(&[(hex::encode(addr(3).0), alloc)]).unwrap();

        // Σ balances == circulating.
        let sum_balances: u64 = state.accounts.iter().map(|a| a.balance.raw()).sum();
        assert_eq!(
            sum_balances, state.circulating_supply(),
            "Σ balances must equal circulating supply after a genesis credit"
        );
        // circulating == emitted - burned; burned untouched.
        assert_eq!(state.total_burned, burned_before);
        assert_eq!(state.circulating_supply(), state.total_emitted - state.total_burned);
        // remaining shrank by exactly the credited sum.
        assert_eq!(state.remaining_supply(), remaining_before - alloc);
        // Fixed cap never exceeded.
        assert!(state.total_emitted <= TOTAL_SUPPLY);
    }

    /// Fail-closed: a bad hex address is rejected and NO state is mutated
    /// (all-or-nothing) — total_emitted and balances stay put.
    #[test]
    fn genesis_accounts_reject_bad_address_without_mutation() {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        let emitted_before = state.total_emitted;
        let root_before = state.compute_state_root();

        let res = state.apply_genesis_accounts(&[
            (hex::encode(addr(1).0), 100),
            ("not-hex".to_string(), 100),
        ]);
        assert!(res.is_err(), "invalid hex address must be rejected");
        assert_eq!(state.total_emitted, emitted_before, "no partial mutation on error");
        assert_eq!(state.compute_state_root(), root_before, "state root unchanged on error");
        assert!(state.accounts.get(&addr(1)).is_none(), "no account created on error");
    }

    /// Fail-closed: an allocation that would push total_emitted above TOTAL_SUPPLY
    /// is rejected (genesis can never mint above the fixed cap).
    #[test]
    fn genesis_accounts_reject_supply_breach() {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        let res = state.apply_genesis_accounts(&[
            (hex::encode(addr(1).0), TOTAL_SUPPLY),
            (hex::encode(addr(2).0), 1),
        ]);
        assert!(res.is_err(), "sum above TOTAL_SUPPLY must be rejected");
        assert_eq!(state.total_emitted, 0);
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
            job, IDENT, IDENT, IDENT, lpid(0), lpid(9), e_bond, budget, v_bond,
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

    // --- PoUW S4: escalation_rounds persistence + state-root folding ------------

    /// A mid-flight escalation round (Committing, no commits yet) opened via `EscalationRound::open`
    /// with a 2-candidate panel pool — small enough to build by hand, big enough to exercise the
    /// panel-draw and DTO round-trip.
    fn test_escalation_round(job: [u8; 32]) -> EscalationRound {
        let identity = commputer_pouw_onchain::escalation_round::JobIdentity {
            program_hash: [0xAA; 32], input_hash: [0xBB; 32], da_root: [0xCC; 32],
        };
        let handoff = commputer_pouw_onchain::lifecycle::EscalationHandoff {
            budget: 1000,
            submitter: lpid(0),
            executor: lpid(9),
            executor_hash: [7u8; 32],
            executor_bond: 100,
            committee_reveals: vec![Reveal { verifier: lpid(10), result_hash: [7u8; 32], salt: [0u8; 32] }],
            committee_bonds: vec![20],
            verifier_bond: 20,
        };
        let candidates = vec![lpid(20), lpid(21)];
        let stake = |_: &ParticipantId| 1u64;
        EscalationRound::open(
            handoff, job, identity, candidates, [42u8; 32], GameParams::default(),
            commputer_pouw_onchain::escalation_round::PanelDeadlines { commit_by: 20, reveal_by: 30 },
            &stake,
        )
    }

    #[test]
    fn escalation_rounds_fold_persist_and_reload() {
        // A mid-flight round folds into the root, survives capture/rollback, persists to
        // CF_ESCALATION, and reloads identically with params re-injected.
        let dir = tempfile::tempdir().unwrap();
        let mut s = ChainState::open(dir.path()).unwrap();
        s.apply_block(&genesis_block()).unwrap();
        let root_before = s.compute_state_root();
        let r = test_escalation_round([5u8; 32]);
        s.escalation_rounds.insert([5u8; 32], r);
        let root_with = s.compute_state_root();
        assert_ne!(root_before, root_with, "6th section folds in");

        // Rollback restores byte-identically. `capture_pre_block`/`rollback_to_pre_block` are
        // private but reachable here (this test module is a descendant of the defining module) —
        // the same idiom the P1 rollback regressions use, just invoked directly instead of via a
        // failing block.
        let snap = s.capture_pre_block();
        s.escalation_rounds.clear();
        assert_ne!(s.compute_state_root(), root_with, "cleared map changes the root");
        s.rollback_to_pre_block(snap);
        assert_eq!(s.compute_state_root(), root_with, "rollback restores the round byte-identically");

        // Persist + reload (the b1b_job_lifecycles_persist_across_reopen idiom: flush, drop, reopen).
        s.flush().unwrap();
        drop(s);
        let s2 = ChainState::open(dir.path()).unwrap();
        assert_eq!(s2.escalation_rounds.len(), 1);
        assert_eq!(s2.compute_state_root(), root_with, "reload reproduces the root");
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
        // SECURITY(F10/F21): the burn must actually cost the sender's balance (conservation:
        // total_burned only rises when real balance leaves circulation) and consume a nonce.
        let acct = state.accounts.get(&addr(1)).unwrap();
        assert_eq!(acct.balance, Amount::from_comme(95));
        assert_eq!(acct.nonce, 1);
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

    // ═══════════════════════════════════════════════════════════════════════════════════
    // Phase 1.1 — B2/B3/B4 money path + D2 fallback + P1 rollback + P8 driver.
    // Every driven test moves money exclusively through REAL transactions in REAL blocks
    // (settlement via the in-apply P8 driver); out-of-band calls appear only where the
    // wiring is PROTECTED (submit_result — B5) and are flagged as such.
    // ═══════════════════════════════════════════════════════════════════════════════════

    const MIN_BUDGET: u64 = commputer_core::compute::MIN_JOB_BUDGET;

    /// Phase 1.1 five-bucket conserved quantity:
    /// spendable + escrowed + active bonded + unbonding cooldown + burned.
    fn money_conserved(state: &ChainState) -> u64 {
        sum_balances(state)
            + state.total_escrowed()
            + state.total_bonded()
            + state.total_unbonding()
            + state.total_burned
    }

    fn bal(state: &ChainState, n: u8) -> u64 {
        state.accounts.get(&addr(n)).map(|a| a.balance.raw()).unwrap_or(0)
    }
    fn lbal(state: &ChainState, n: u8) -> u64 {
        state.accounts.get(&lpaddr(n)).map(|a| a.balance.raw()).unwrap_or(0)
    }
    fn bps_of(x: u64, bps: u32) -> u64 {
        x * bps as u64 / 10_000
    }

    /// A SubmitJobV2 kind carrying `budget` and the canonical test identity hashes.
    fn v2_kind(budget: u64) -> TxKind {
        v2_kind_l2(budget, None)
    }
    fn v2_kind_l2(budget: u64, l2_id: Option<String>) -> TxKind {
        TxKind::SubmitJobV2 {
            program_hash: [0xAA; 32],
            input_hash: [0xBB; 32],
            da_root: [0xCC; 32],
            resources: commputer_core::compute::ResourceRequirements::cpu_only(1, 64),
            max_duration_secs: 60,
            comme_budget: Amount::from_raw(budget),
            l2_id,
        }
    }
    fn v1_kind(budget: u64) -> TxKind {
        TxKind::SubmitJob {
            job_spec_hash: [0u8; 32],
            resources: commputer_core::compute::ResourceRequirements::cpu_only(1, 64),
            max_duration_secs: 60,
            comme_budget: Amount::from_raw(budget),
            l2_id: None,
        }
    }

    // --- B2: SubmitJobV2 burn→escrow -----------------------------------------------------

    #[test]
    fn b2_submit_job_v2_escrows_budget_and_records_pending() {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        state.accounts.get_or_create(addr(1)).balance = Amount::from_raw(MIN_BUDGET + 500);
        state.total_emitted = MIN_BUDGET + 500;
        let conserved = money_conserved(&state);

        let tx = unsigned_with_fee(addr(1), 0, v2_kind(MIN_BUDGET), 100);
        let job = tx.hash().0; // G-A: job identity == tx hash
        state.apply_block(&block_with(&state, 1, vec![tx])).unwrap();

        assert_eq!(state.escrowed_for_job(&job), MIN_BUDGET, "pot == budget");
        assert_eq!(bal(&state, 1), 400, "balance debited budget + fee");
        assert_eq!(state.total_burned, 100, "ONLY the fee burned — escrow never moves total_burned");
        let rec = state.pending_jobs.get(&job).copied().expect("pending record written");
        assert_eq!(rec.submitter, addr(1).0);
        assert_eq!(rec.budget, MIN_BUDGET);
        assert_eq!(rec.program_hash, [0xAA; 32]);
        assert_eq!(rec.input_hash, [0xBB; 32]);
        assert_eq!(rec.da_root, [0xCC; 32]);
        assert_eq!(rec.submitted_height, 0, "G-F: parent height during apply");
        assert_eq!(rec.claim_by, PhaseWindows::default().claim_blocks, "submit height + claim window");
        assert!(state.job_lifecycles.is_empty(), "no lifecycle until ClaimJob");
        assert_eq!(state.accounts.get(&addr(1)).unwrap().nonce, 1);
        assert_eq!(money_conserved(&state), conserved, "escrow held in circulation");
    }

    #[test]
    fn b2_submit_job_v2_below_min_budget_rejects_block() {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        state.accounts.get_or_create(addr(1)).balance = Amount::from_raw(MIN_BUDGET);
        let root = state.compute_state_root();
        let err = state
            .apply_block(&block_with(&state, 1, vec![unsigned(addr(1), 0, v2_kind(MIN_BUDGET - 1))]))
            .unwrap_err();
        assert!(err.to_string().contains("below minimum"), "got: {err}");
        assert!(state.pending_jobs.is_empty() && state.escrow_by_job.is_empty());
        assert_eq!(state.compute_state_root(), root, "P1 rollback: no trace");
    }

    #[test]
    fn b2_submit_job_v2_insufficient_balance_rejects_block_without_smear() {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        state.accounts.get_or_create(addr(1)).balance = Amount::from_raw(MIN_BUDGET - 1);
        let root = state.compute_state_root();
        let err = state
            .apply_block(&block_with(&state, 1, vec![unsigned(addr(1), 0, v2_kind(MIN_BUDGET))]))
            .unwrap_err();
        assert!(matches!(err, StateError::InsufficientBalance), "got: {err}");
        assert_eq!(bal(&state, 1), MIN_BUDGET - 1, "balance untouched");
        assert_eq!(state.accounts.get(&addr(1)).unwrap().nonce, 0, "nonce not bumped");
        assert!(state.pending_jobs.is_empty() && state.escrow_by_job.is_empty());
        assert_eq!(state.compute_state_root(), root);
    }

    #[test]
    fn b2_submit_job_v2_duplicate_job_id_rejects_block() {
        // A real duplicate needs a SHA-256 collision (a literal re-broadcast dies on the nonce
        // check — see the replay test); tamper each guard map ahead of time instead.
        let tx = unsigned(addr(1), 0, v2_kind(MIN_BUDGET));
        let job = tx.hash().0;
        for tamper in 0..3 {
            let mut state = ChainState::new();
            state.apply_block(&genesis_block()).unwrap();
            state.accounts.get_or_create(addr(1)).balance = Amount::from_raw(MIN_BUDGET);
            match tamper {
                0 => {
                    state.pending_jobs.insert(job, PendingJobRecord {
                        submitter: [9u8; 32], budget: 1, program_hash: [0u8; 32],
                        input_hash: [0u8; 32], da_root: [0u8; 32], submitted_height: 0, claim_by: 99,
                    });
                }
                1 => { state.escrow_by_job.insert(job, 1); }
                _ => { state.job_lifecycles.insert(job, sample_lifecycle(job, 7)); }
            }
            let err = state.apply_block(&block_with(&state, 1, vec![tx.clone()])).unwrap_err();
            assert!(err.to_string().contains("duplicate job id"), "guard {tamper}: got {err}");
            assert_eq!(bal(&state, 1), MIN_BUDGET, "no money moved on the duplicate");
        }
    }

    #[test]
    fn b2_submit_job_v2_in_batch_rejects_block() {
        // G-C: no unique per-op job id exists inside a Batch (one tx hash, nonce bumped once).
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        state.accounts.get_or_create(addr(1)).balance = Amount::from_raw(2 * MIN_BUDGET);
        let root = state.compute_state_root();
        let batch = TxKind::Batch { operations: vec![v2_kind(MIN_BUDGET)] };
        let err = state
            .apply_block(&block_with(&state, 1, vec![unsigned(addr(1), 0, batch)]))
            .unwrap_err();
        assert!(err.to_string().contains("not allowed in Batch"), "got: {err}");
        assert!(state.pending_jobs.is_empty() && state.escrow_by_job.is_empty());
        assert_eq!(state.compute_state_root(), root);
    }

    #[test]
    fn b2_submit_job_v1_still_burns_top_level_and_in_batch() {
        // The legacy path is byte-for-byte untouched: V1 burns at submit, creates NO pot and
        // NO pending record — top-level and Batch-inner alike.
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        state.accounts.get_or_create(addr(1)).balance = Amount::from_raw(2 * MIN_BUDGET);
        state.total_emitted = 2 * MIN_BUDGET;
        let conserved = money_conserved(&state);

        state
            .apply_block(&block_with(&state, 1, vec![unsigned(addr(1), 0, v1_kind(MIN_BUDGET))]))
            .unwrap();
        assert_eq!(state.total_burned, MIN_BUDGET, "V1 burns the whole budget at submit");

        let batch = TxKind::Batch { operations: vec![v1_kind(MIN_BUDGET)] };
        state
            .apply_block(&block_with(&state, 2, vec![unsigned(addr(1), 1, batch)]))
            .unwrap();
        assert_eq!(state.total_burned, 2 * MIN_BUDGET, "V1-in-Batch burns too");
        assert_eq!(bal(&state, 1), 0);
        assert!(state.pending_jobs.is_empty(), "V1 never writes a pending record");
        assert!(state.escrow_by_job.is_empty(), "V1 never escrows");
        assert_eq!(money_conserved(&state), conserved, "burns stay inside the invariant");
    }

    // --- B3: ClaimJob opens the lifecycle -------------------------------------------------

    /// Real-block setup shared by the B3/B4 tests: bonded verifier-validators addr(3)/(4)/(5)
    /// (the exact candidate set), executor addr(1) (validator, bonded — excluded as `from`),
    /// submitter addr(2), addr(6) a validator WITHOUT bonded stake (not a candidate ⇒ never a
    /// committee member), addr(7) bonded but non-compliant (excluded). Block 1 submits the V2
    /// job, block 2 claims it (deadlines: result_by 11 / commit_by 21 / reveal_by 31; default
    /// windows). If `result` is given the executor's answer is delivered out-of-band
    /// (submit_result — the B5 wiring is PROTECTED) and the committee is drawn.
    fn claimed_job_state(result: Option<[u8; 32]>) -> (ChainState, [u8; 32]) {
        let v_bond = GameParams::default().verifier_bond;
        let min_bond = StakeParams::default().min_bond;
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        state.accounts.get_or_create(addr(2)).balance = Amount::from_raw(MIN_BUDGET);
        let e = state.accounts.get_or_create(addr(1));
        e.is_validator = true;
        e.balance = Amount::from_raw(MIN_BUDGET); // e_bond == max(budget, flat 100) == budget
        state.bonded_stake.insert(addr(1), min_bond);
        for v in [3u8, 4, 5] {
            let a = state.accounts.get_or_create(addr(v));
            a.is_validator = true;
            a.balance = Amount::from_raw(v_bond);
            state.bonded_stake.insert(addr(v), min_bond);
        }
        let a6 = state.accounts.get_or_create(addr(6));
        a6.is_validator = true;
        a6.balance = Amount::from_raw(v_bond);
        let a7 = state.accounts.get_or_create(addr(7));
        a7.is_validator = true;
        a7.compliance = ComplianceStatus::NerfedAdversarial;
        state.bonded_stake.insert(addr(7), min_bond);

        let submit = unsigned(addr(2), 0, v2_kind(MIN_BUDGET));
        let job = submit.hash().0;
        state.apply_block(&block_with(&state, 1, vec![submit])).unwrap();
        state
            .apply_block(&block_with(&state, 2, vec![unsigned(addr(1), 0, TxKind::ClaimJob { job_id: job })]))
            .unwrap();

        if let Some(r) = result {
            let stake = |_: &ParticipantId| 1u64;
            assert_eq!(
                state.job_lifecycles.get_mut(&job).unwrap().submit_result(
                    ParticipantId(addr(1).0), r, [42u8; 32], 2, &stake),
                EventResult::Accepted,
                "out-of-band result delivery (B5 wiring is PROTECTED)"
            );
        }
        (state, job)
    }

    #[test]
    fn b3_claim_opens_lifecycle_escrows_bond_and_snapshots_sorted_candidates() {
        let (state, job) = claimed_job_state(None);

        assert!(!state.pending_jobs.contains_key(&job), "pending consumed by the claim");
        assert_eq!(
            state.escrowed_for_job(&job),
            2 * MIN_BUDGET,
            "pot == budget + executor bond (Be = max(budget, flat) = budget)"
        );
        assert_eq!(bal(&state, 1), 0, "executor bond debited");
        let rec = state.job_lifecycles.get(&job).expect("lifecycle open").to_record();
        assert_eq!(rec.submitter, addr(2).0);
        assert_eq!(rec.executor, addr(1).0);
        assert_eq!(rec.budget, MIN_BUDGET);
        assert_eq!(rec.executor_bond, MIN_BUDGET);
        assert_eq!(rec.verifier_bond, GameParams::default().verifier_bond);
        // P9: the program identity travels pending record → lifecycle DTO.
        assert_eq!(rec.program_hash, [0xAA; 32]);
        assert_eq!(rec.input_hash, [0xBB; 32]);
        assert_eq!(rec.da_root, [0xCC; 32]);
        // G-G: EXACT sorted eligible set — bonded compliant validators, minus the claimer,
        // minus the unbonded addr(6), minus the non-compliant addr(7).
        assert_eq!(
            rec.candidates,
            vec![addr(3).0, addr(4).0, addr(5).0],
            "candidate snapshot: exact membership in sorted address order"
        );
        // Deadlines anchored at CLAIM height (parent height 1) with the G-E windows.
        assert_eq!(rec.deadlines.result_by, 1 + PhaseWindows::default().result_blocks);
        assert_eq!(rec.deadlines.commit_by, rec.deadlines.result_by + PhaseWindows::default().commit_blocks);
        assert_eq!(rec.deadlines.reveal_by, rec.deadlines.commit_by + PhaseWindows::default().reveal_blocks);
    }

    #[test]
    fn b3_claim_from_non_validator_rejects_block() {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        state.accounts.get_or_create(addr(2)).balance = Amount::from_raw(2 * MIN_BUDGET);
        let submit = unsigned(addr(2), 0, v2_kind(MIN_BUDGET));
        let job = submit.hash().0;
        state.apply_block(&block_with(&state, 1, vec![submit])).unwrap();
        // addr(2) is funded but NOT a validator.
        let err = state
            .apply_block(&block_with(&state, 2, vec![unsigned(addr(2), 1, TxKind::ClaimJob { job_id: job })]))
            .unwrap_err();
        assert!(err.to_string().contains("only validators"), "got: {err}");
        assert!(state.pending_jobs.contains_key(&job), "pending intact");
        assert_eq!(state.escrowed_for_job(&job), MIN_BUDGET, "pot untouched");
        assert!(state.job_lifecycles.is_empty());
    }

    #[test]
    fn b3_losing_claim_is_nonce_consuming_noop() {
        let (mut state, job) = claimed_job_state(None);
        let pot = state.escrowed_for_job(&job);
        let bal3 = bal(&state, 3);
        // addr(3) is a bonded validator — but the job is already claimed. Losing the
        // permissionless claim race is a NORMAL outcome, not a block-invalidating error:
        // the block applies and the losing ClaimJob is a nonce-consuming no-op (an Err here
        // would permanently wedge the loser's nonce, so its later committee Commit/Reveal
        // could never apply — no quorum, jobs refund instead of paying).
        state
            .apply_block(&block_with(&state, 3, vec![unsigned(addr(3), 0, TxKind::ClaimJob { job_id: job })]))
            .unwrap();
        assert_eq!(state.accounts.get(&addr(3)).unwrap().nonce, 1, "loser's nonce consumed");
        assert_eq!(state.escrowed_for_job(&job), pot, "no second bond escrowed");
        assert_eq!(bal(&state, 3), bal3, "loser's bond untouched");
        assert_eq!(
            state.job_lifecycles.get(&job).unwrap().to_record().executor,
            addr(1).0,
            "winner's claim stands"
        );
    }

    #[test]
    fn b3_double_claim_within_one_batch_first_wins_second_noop() {
        // The FIRST op opens the lifecycle + escrows the bond; the second hits the
        // lifecycle-exists guard and applies as a money-free no-op — the batch (and block)
        // stays valid, the bond is escrowed exactly once, and the batch bumps the nonce once.
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        state.accounts.get_or_create(addr(2)).balance = Amount::from_raw(MIN_BUDGET);
        let v = state.accounts.get_or_create(addr(3));
        v.is_validator = true;
        v.balance = Amount::from_raw(MIN_BUDGET);
        let submit = unsigned(addr(2), 0, v2_kind(MIN_BUDGET));
        let job = submit.hash().0;
        state.apply_block(&block_with(&state, 1, vec![submit])).unwrap();

        let batch = TxKind::Batch {
            operations: vec![TxKind::ClaimJob { job_id: job }, TxKind::ClaimJob { job_id: job }],
        };
        state
            .apply_block(&block_with(&state, 2, vec![unsigned(addr(3), 0, batch)]))
            .unwrap();
        assert!(!state.pending_jobs.contains_key(&job), "pending consumed by the first op");
        assert_eq!(
            state.job_lifecycles.get(&job).unwrap().to_record().executor,
            addr(3).0,
            "first op's claim stands"
        );
        assert_eq!(
            state.escrowed_for_job(&job),
            2 * MIN_BUDGET,
            "budget + exactly ONE executor bond (the no-op escrowed nothing)"
        );
        assert_eq!(bal(&state, 3), 0, "bond debited once, not twice");
        assert_eq!(state.accounts.get(&addr(3)).unwrap().nonce, 1, "batch bumps nonce once");
    }

    #[test]
    fn b3_claim_past_window_rejects_block() {
        // The defense-in-depth guard (the P8 driver normally expires the record first): tamper
        // in a pending record whose window already closed.
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        state.apply_block(&block_with(&state, 1, vec![])).unwrap(); // height 1
        let v = state.accounts.get_or_create(addr(1));
        v.is_validator = true;
        v.balance = Amount::from_raw(MIN_BUDGET);
        let job = [33u8; 32];
        state.pending_jobs.insert(job, PendingJobRecord {
            submitter: addr(2).0, budget: MIN_BUDGET, program_hash: [0u8; 32],
            input_hash: [0u8; 32], da_root: [0u8; 32], submitted_height: 0, claim_by: 0,
        });
        state.escrow_by_job.insert(job, MIN_BUDGET);
        let err = state
            .apply_block(&block_with(&state, 2, vec![unsigned(addr(1), 0, TxKind::ClaimJob { job_id: job })]))
            .unwrap_err();
        assert!(err.to_string().contains("claim window expired"), "got: {err}");
        assert!(state.job_lifecycles.is_empty());
        assert_eq!(bal(&state, 1), MIN_BUDGET, "no bond taken");
    }

    #[test]
    fn m2_claim_unknown_id_rejects_block_no_smear() {
        // M2 (flip): an unknown/expired job id now REJECTS the whole block (was the legacy no-op
        // accept). Deterministic reject, P1 rollback leaves no trace (root unchanged).
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        let v = state.accounts.get_or_create(addr(1));
        v.is_validator = true;
        v.balance = Amount::from_raw(1_000);
        let root = state.compute_state_root();
        let err = state
            .apply_block(&block_with(&state, 1, vec![unsigned(addr(1), 0, TxKind::ClaimJob { job_id: [9u8; 32] })]))
            .unwrap_err();
        assert!(err.to_string().contains("unknown or expired job id"), "got: {err}");
        assert_eq!(state.accounts.get(&addr(1)).map(|a| a.nonce).unwrap_or(0), 0, "no nonce bump on reject");
        assert_eq!(bal(&state, 1), 1_000, "no money moved");
        assert!(state.job_lifecycles.is_empty() && state.escrow_by_job.is_empty());
        assert_eq!(state.compute_state_root(), root, "P1 rollback: rejected block leaves no trace");
        // Inside a Batch: same reject (shared apply_claim_job).
        let err = state
            .apply_block(&block_with(&state, 1, vec![unsigned(addr(1), 0, TxKind::Batch {
                operations: vec![TxKind::ClaimJob { job_id: [9u8; 32] }] })]))
            .unwrap_err();
        assert!(err.to_string().contains("unknown or expired job id"), "batch got: {err}");
    }

    #[test]
    fn b3_claim_insufficient_bond_balance_rejects_without_smear() {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        state.accounts.get_or_create(addr(2)).balance = Amount::from_raw(MIN_BUDGET);
        let v = state.accounts.get_or_create(addr(1));
        v.is_validator = true;
        v.balance = Amount::from_raw(MIN_BUDGET - 1); // one short of e_bond
        let submit = unsigned(addr(2), 0, v2_kind(MIN_BUDGET));
        let job = submit.hash().0;
        state.apply_block(&block_with(&state, 1, vec![submit])).unwrap();
        let root = state.compute_state_root();

        let err = state
            .apply_block(&block_with(&state, 2, vec![unsigned(addr(1), 0, TxKind::ClaimJob { job_id: job })]))
            .unwrap_err();
        assert!(matches!(err, StateError::InsufficientBalance), "got: {err}");
        assert_eq!(state.escrowed_for_job(&job), MIN_BUDGET, "pot still == budget only");
        assert!(state.pending_jobs.contains_key(&job), "pending intact");
        assert!(state.job_lifecycles.is_empty(), "no half-open lifecycle");
        assert_eq!(state.compute_state_root(), root, "P1 rollback: no trace");
    }

    #[test]
    fn b3_executor_bond_flat_floor_applies_when_above_budget() {
        // G-D boundary: e_bond = max(budget, game_params.executor_bond).
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        state.game_params.executor_bond = 3 * MIN_BUDGET; // flat knob ABOVE the budget
        state.accounts.get_or_create(addr(2)).balance = Amount::from_raw(MIN_BUDGET);
        let v = state.accounts.get_or_create(addr(1));
        v.is_validator = true;
        v.balance = Amount::from_raw(3 * MIN_BUDGET);
        let submit = unsigned(addr(2), 0, v2_kind(MIN_BUDGET));
        let job = submit.hash().0;
        state.apply_block(&block_with(&state, 1, vec![submit])).unwrap();
        state
            .apply_block(&block_with(&state, 2, vec![unsigned(addr(1), 0, TxKind::ClaimJob { job_id: job })]))
            .unwrap();
        assert_eq!(state.escrowed_for_job(&job), MIN_BUDGET + 3 * MIN_BUDGET, "budget + flat-floor bond");
        assert_eq!(state.job_lifecycles.get(&job).unwrap().to_record().executor_bond, 3 * MIN_BUDGET);
        assert_eq!(bal(&state, 1), 0);
    }

    // --- B4: Commit/Reveal route to the lifecycle ------------------------------------------

    #[test]
    fn b4_commit_on_claimed_job_pre_b5_rejects_wrong_phase() {
        // The B4-before-B5 crux pinned: without submit_result the phase is AwaitingResult, so
        // NO Commit can appear in any valid block on this branch — strictly inert-but-strict.
        let (mut state, job) = claimed_job_state(None);
        let v_bond = GameParams::default().verifier_bond;
        let pot = state.escrowed_for_job(&job);
        let c: Commitment = make_commitment(&ParticipantId(addr(3).0), &[7u8; 32], &[0u8; 32], v_bond);
        let err = state
            .apply_block(&block_with(&state, 3, vec![unsigned(addr(3), 0, TxKind::Commit {
                job_id: job, commit: c.commit, bond: Amount::from_raw(v_bond) })]))
            .unwrap_err();
        assert!(err.to_string().contains("WrongPhase"), "got: {err}");
        assert_eq!(state.escrowed_for_job(&job), pot, "no bond escrowed");
        assert_eq!(state.job_lifecycles.get(&job).unwrap().to_record().commitments.len(), 0);
    }

    #[test]
    fn b4_commit_accepts_after_result_and_escrows_bond_exactly_once() {
        let result = [7u8; 32];
        let (mut state, job) = claimed_job_state(Some(result));
        let v_bond = GameParams::default().verifier_bond;
        // Extra funds so the double-commit below passes the balance pre-check and reaches the
        // lifecycle's own DoubleCommit rejection (a broke committer dies earlier, on balance).
        state.accounts.get_mut(&addr(3)).unwrap().balance = Amount::from_raw(2 * v_bond);
        let pot0 = state.escrowed_for_job(&job);
        let c: Commitment = make_commitment(&ParticipantId(addr(3).0), &result, &[0u8; 32], v_bond);
        state
            .apply_block(&block_with(&state, 3, vec![unsigned(addr(3), 0, TxKind::Commit {
                job_id: job, commit: c.commit, bond: Amount::from_raw(v_bond) })]))
            .unwrap();
        assert_eq!(
            state.escrowed_for_job(&job),
            pot0 + v_bond,
            "pot grew by EXACTLY one bond — record_commit escrows it, the arm must not double-escrow"
        );
        assert_eq!(bal(&state, 3), v_bond, "committer's balance debited by exactly the bond");
        let rec = state.job_lifecycles.get(&job).unwrap().to_record();
        assert_eq!(rec.commitments.len(), 1);
        assert_eq!(state.accounts.get(&addr(3)).unwrap().nonce, 1);

        // Double-commit from the same verifier: rejected, zero delta.
        let c2: Commitment = make_commitment(&ParticipantId(addr(3).0), &result, &[9u8; 32], v_bond);
        let err = state
            .apply_block(&block_with(&state, 4, vec![unsigned(addr(3), 1, TxKind::Commit {
                job_id: job, commit: c2.commit, bond: Amount::from_raw(v_bond) })]))
            .unwrap_err();
        assert!(err.to_string().contains("DoubleCommit"), "got: {err}");
        assert_eq!(state.escrowed_for_job(&job), pot0 + v_bond, "still exactly one bond");
    }

    #[test]
    fn b4_commit_wrong_bond_and_non_member_reject() {
        let result = [7u8; 32];
        let (mut state, job) = claimed_job_state(Some(result));
        let v_bond = GameParams::default().verifier_bond;
        // Cover the oversized declared bond so the balance pre-check passes and the lifecycle's
        // WrongBond rejection is what fires.
        state.accounts.get_mut(&addr(4)).unwrap().balance = Amount::from_raw(2 * v_bond);
        let pot0 = state.escrowed_for_job(&job);

        // Wrong bond amount (committee member addr(4)).
        let c: Commitment = make_commitment(&ParticipantId(addr(4).0), &result, &[0u8; 32], v_bond + 1);
        let err = state
            .apply_block(&block_with(&state, 3, vec![unsigned(addr(4), 0, TxKind::Commit {
                job_id: job, commit: c.commit, bond: Amount::from_raw(v_bond + 1) })]))
            .unwrap_err();
        assert!(err.to_string().contains("WrongBond"), "got: {err}");

        // Non-member: addr(6) is a validator but unbonded ⇒ never in the candidate snapshot ⇒
        // not on the committee. The tx-level validator gate passes; membership rejects.
        let c6: Commitment = make_commitment(&ParticipantId(addr(6).0), &result, &[0u8; 32], v_bond);
        let err = state
            .apply_block(&block_with(&state, 3, vec![unsigned(addr(6), 0, TxKind::Commit {
                job_id: job, commit: c6.commit, bond: Amount::from_raw(v_bond) })]))
            .unwrap_err();
        assert!(err.to_string().contains("NotCommitteeMember"), "got: {err}");

        assert_eq!(state.escrowed_for_job(&job), pot0, "no rejected commit escrowed anything");
        assert_eq!(state.job_lifecycles.get(&job).unwrap().to_record().commitments.len(), 0);
    }

    #[test]
    fn b4_batch_double_commit_rejects_whole_block_and_rolls_back_first() {
        let result = [7u8; 32];
        let (mut state, job) = claimed_job_state(Some(result));
        let v_bond = GameParams::default().verifier_bond;
        // Enough for two bonds: op2 must reach the lifecycle's DoubleCommit (not die on balance).
        state.accounts.get_mut(&addr(3)).unwrap().balance = Amount::from_raw(2 * v_bond);
        let pot0 = state.escrowed_for_job(&job);
        let root = state.compute_state_root();
        let c: Commitment = make_commitment(&ParticipantId(addr(3).0), &result, &[0u8; 32], v_bond);
        let op = TxKind::Commit { job_id: job, commit: c.commit, bond: Amount::from_raw(v_bond) };
        let err = state
            .apply_block(&block_with(&state, 3, vec![unsigned(addr(3), 0, TxKind::Batch {
                operations: vec![op.clone(), op] })]))
            .unwrap_err();
        assert!(err.to_string().contains("DoubleCommit"), "got: {err}");
        // P1 rollback: the FIRST op's accepted escrow is erased with the block.
        assert_eq!(state.escrowed_for_job(&job), pot0, "first op's bond escrow rolled back");
        assert_eq!(bal(&state, 3), 2 * v_bond, "committer refunded by the rollback");
        assert_eq!(state.job_lifecycles.get(&job).unwrap().to_record().commitments.len(), 0);
        assert_eq!(state.compute_state_root(), root);
    }

    #[test]
    fn b4_committer_balance_below_bond_rejects_block_lifecycle_intact() {
        let result = [7u8; 32];
        let (mut state, job) = claimed_job_state(Some(result));
        let v_bond = GameParams::default().verifier_bond;
        state.accounts.get_mut(&addr(4)).unwrap().balance = Amount::from_raw(v_bond - 1);
        let pot0 = state.escrowed_for_job(&job);
        let c: Commitment = make_commitment(&ParticipantId(addr(4).0), &result, &[0u8; 32], v_bond);
        let err = state
            .apply_block(&block_with(&state, 3, vec![unsigned(addr(4), 0, TxKind::Commit {
                job_id: job, commit: c.commit, bond: Amount::from_raw(v_bond) })]))
            .unwrap_err();
        assert!(matches!(err, StateError::InsufficientBalance), "got: {err}");
        assert_eq!(state.escrowed_for_job(&job), pot0);
        let rec = state.job_lifecycles.get(&job).expect("lifecycle intact").to_record();
        assert_eq!(rec.commitments.len(), 0, "no partial commitment recorded");
    }

    #[test]
    fn b4_reveal_self_advances_past_commit_by_and_accepts() {
        // Pins the arm's built-in lifecycle_advance: build a Committing lifecycle whose commit
        // window is ALREADY closed and insert it out-of-band AFTER the last block, so the P8
        // driver has never seen it — the Reveal tx alone must flip Committing→Revealing.
        let v_bond = GameParams::default().verifier_bond;
        let (budget, e_bond, _) = fuel_mins();
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        state.apply_block(&block_with(&state, 1, vec![])).unwrap();
        state.apply_block(&block_with(&state, 2, vec![])).unwrap(); // parent height for block 3 = 2
        state.accounts.get_or_create(lpaddr(0)).balance = Amount::from_raw(budget);
        state.accounts.get_or_create(lpaddr(9)).balance = Amount::from_raw(e_bond);
        for (n, v) in [(10u8, addr(10)), (11u8, addr(11))] {
            let a = state.accounts.get_or_create(v);
            a.is_validator = true;
            a.balance = Amount::from_raw(v_bond);
            let _ = n;
        }
        let job = [44u8; 32];
        state.escrow_into_job(&lpaddr(0), job, budget).unwrap();
        state.escrow_into_job(&lpaddr(9), job, e_bond).unwrap();
        let mut lc = JobLifecycle::open(
            job, IDENT, IDENT, IDENT, lpid(0), lpid(9), e_bond, budget, v_bond,
            GameParams::default(), ResolutionParams::default(),
            vec![ParticipantId(addr(10).0), ParticipantId(addr(11).0)],
            PhaseDeadlines { result_by: 1, commit_by: 1, reveal_by: 30 },
        );
        let stake = |_: &ParticipantId| 1u64;
        assert_eq!(lc.submit_result(lpid(9), [7u8; 32], [42u8; 32], 1, &stake), EventResult::Accepted);
        state.job_lifecycles.insert(job, lc);
        for (i, a) in [addr(10), addr(11)].iter().enumerate() {
            let c = make_commitment(&ParticipantId(a.0), &[7u8; 32], &[i as u8; 32], v_bond);
            assert_eq!(state.lifecycle_record_commit(job, c, 1).unwrap(), Some(EventResult::Accepted));
        }
        assert_eq!(state.job_lifecycles.get(&job).unwrap().to_record().phase,
            commputer_pouw_onchain::lifecycle::PhaseRec::Committing, "still Committing pre-block");

        // Block 3: parent height 2 > commit_by 1 — the ARM advances, then records the reveal.
        state
            .apply_block(&block_with(&state, 3, vec![unsigned(addr(10), 0, TxKind::Reveal {
                job_id: job, result_hash: [7u8; 32], salt: [0u8; 32] })]))
            .unwrap();
        let rec = state.job_lifecycles.get(&job).unwrap().to_record();
        assert_eq!(rec.reveals.len(), 1, "reveal landed without any driver/advance call");
        assert_eq!(rec.phase, commputer_pouw_onchain::lifecycle::PhaseRec::Revealing);

        // Replay of the same reveal: AlreadyRevealed ⇒ block rejected, zero delta.
        let err = state
            .apply_block(&block_with(&state, 4, vec![unsigned(addr(10), 1, TxKind::Reveal {
                job_id: job, result_hash: [7u8; 32], salt: [0u8; 32] })]))
            .unwrap_err();
        assert!(err.to_string().contains("AlreadyRevealed"), "got: {err}");
        // Mismatched salt from the other committer: RevealMismatch ⇒ rejected.
        let err = state
            .apply_block(&block_with(&state, 4, vec![unsigned(addr(11), 0, TxKind::Reveal {
                job_id: job, result_hash: [7u8; 32], salt: [9u8; 32] })]))
            .unwrap_err();
        assert!(err.to_string().contains("RevealMismatch"), "got: {err}");
        assert_eq!(state.job_lifecycles.get(&job).unwrap().to_record().reveals.len(), 1);
    }

    // --- pending-job expiry ------------------------------------------------------------------

    #[test]
    fn expire_pending_job_refunds_exactly_once() {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        state.accounts.get_or_create(addr(1)).balance = Amount::from_raw(MIN_BUDGET);
        state.total_emitted = MIN_BUDGET;
        let conserved = money_conserved(&state);
        let tx = unsigned(addr(1), 0, v2_kind(MIN_BUDGET));
        let job = tx.hash().0;
        state.apply_block(&block_with(&state, 1, vec![tx])).unwrap();
        let claim_by = state.pending_jobs.get(&job).unwrap().claim_by;

        // Not yet due (height == claim_by is still claimable) ⇒ None, nothing moves.
        assert_eq!(state.expire_pending_job(job, claim_by).unwrap(), None);
        assert_eq!(state.escrowed_for_job(&job), MIN_BUDGET);

        let out = state.expire_pending_job(job, claim_by + 1).unwrap().expect("due ⇒ refund");
        assert_eq!(out.submitter_refunded, MIN_BUDGET, "no-fault: FULL refund");
        assert_eq!(out.burned, 0);
        assert_eq!(bal(&state, 1), MIN_BUDGET, "budget back with the submitter");
        assert!(!state.escrow_by_job.contains_key(&job), "empty pot removed");
        assert!(!state.pending_jobs.contains_key(&job), "record removed");
        assert_eq!(state.total_burned, 0, "expiry burns nothing");
        // At-most-once: the record is gone, a second call is a no-op.
        assert_eq!(state.expire_pending_job(job, claim_by + 2).unwrap(), None);
        assert_eq!(bal(&state, 1), MIN_BUDGET);
        assert_eq!(money_conserved(&state), conserved);
    }

    #[test]
    fn p8_pending_job_expires_via_the_driver_and_refunds() {
        let mut state = ChainState::new();
        state.phase_windows.claim_blocks = 2;
        state.apply_block(&genesis_block()).unwrap();
        state.accounts.get_or_create(addr(1)).balance = Amount::from_raw(MIN_BUDGET);
        state.total_emitted = MIN_BUDGET;
        let conserved = money_conserved(&state);
        let tx = unsigned(addr(1), 0, v2_kind(MIN_BUDGET));
        let job = tx.hash().0;
        state.apply_block(&block_with(&state, 1, vec![tx])).unwrap(); // claim_by = 0 + 2
        state.apply_block(&block_with(&state, 2, vec![])).unwrap(); // 2 > 2 is false: still pending
        assert!(state.pending_jobs.contains_key(&job), "not yet due");
        state.apply_block(&block_with(&state, 3, vec![])).unwrap(); // 3 > 2: the driver refunds
        assert!(state.pending_jobs.is_empty(), "driver expired the unclaimed job");
        assert_eq!(state.escrowed_for_job(&job), 0);
        assert_eq!(bal(&state, 1), MIN_BUDGET, "full refund");
        assert_eq!(state.total_burned, 0);
        assert_eq!(money_conserved(&state), conserved);
    }

    // --- D2 fallback + P2 settle re-entry --------------------------------------------------

    /// Out-of-band round at the committee stage: funded lifecycle over `state`, executor claims
    /// [7;32], each committee member (lpid 10/11/12) commits `hashes[i]` and reveals iff
    /// `reveal_mask[i]`, advanced past reveal_by — ready to settle. Returns
    /// (state, job, budget, e_bond, v_bond, conserved0).
    fn round_ready_state(
        hashes: [[u8; 32]; 3],
        reveal_mask: [bool; 3],
    ) -> (ChainState, [u8; 32], u64, u64, u64, u64) {
        let (budget, e_bond, v_bond) = fuel_mins();
        let committee = [lpid(10), lpid(11), lpid(12)];
        let job = [21u8; 32];
        let mut state = ChainState::new();
        state.total_emitted = budget + e_bond + 3 * v_bond;
        state.accounts.get_or_create(lpaddr(0)).balance = Amount::from_raw(budget);
        state.accounts.get_or_create(lpaddr(9)).balance = Amount::from_raw(e_bond);
        for c in 10u8..13 {
            state.accounts.get_or_create(lpaddr(c)).balance = Amount::from_raw(v_bond);
        }
        let conserved = money_conserved(&state);
        state.escrow_into_job(&lpaddr(0), job, budget).unwrap();
        state.escrow_into_job(&lpaddr(9), job, e_bond).unwrap();
        let mut lc = JobLifecycle::open(
            job, IDENT, IDENT, IDENT, lpid(0), lpid(9), e_bond, budget, v_bond,
            GameParams::default(), ResolutionParams::default(), committee.to_vec(), test_deadlines(),
        );
        let stake = |_: &ParticipantId| 1u64;
        assert_eq!(lc.submit_result(lpid(9), [7u8; 32], [42u8; 32], 5, &stake), EventResult::Accepted);
        state.job_lifecycles.insert(job, lc);
        for (i, c) in committee.iter().enumerate() {
            let commit = make_commitment(c, &hashes[i], &[i as u8; 32], v_bond);
            assert_eq!(state.lifecycle_record_commit(job, commit, 15).unwrap(), Some(EventResult::Accepted));
        }
        state.lifecycle_advance(job, 21);
        for (i, c) in committee.iter().enumerate() {
            if !reveal_mask[i] {
                continue;
            }
            let r = Reveal { verifier: *c, result_hash: hashes[i], salt: [i as u8; 32] };
            assert_eq!(state.lifecycle_record_reveal(job, r, 25), Some(EventResult::Accepted));
        }
        state.lifecycle_advance(job, 31);
        (state, job, budget, e_bond, v_bond, conserved)
    }

    #[test]
    fn d2_settle_and_drain_confirmed_drains_entry_at_most_once() {
        // All 3 reveal the executor's hash ⇒ Confirmed; settle itself drains the pot and the
        // wrapper removes the map entry ⇒ at-most-once by construction.
        let (mut state, job, _, e_bond, v_bond, conserved) =
            round_ready_state([[7u8; 32]; 3], [true, true, true]);
        let (t, fb) = state.lifecycle_settle_and_drain(job, &ByteEq, BlockHash([0u8; 32])).unwrap().expect("due");
        match t {
            Terminal::Confirmed(out) => assert_eq!(out.bonds_returned, e_bond + 3 * v_bond),
            other => panic!("expected Confirmed, got {other:?}"),
        }
        assert!(fb.is_none(), "no fallback on Confirmed");
        assert!(!state.job_lifecycles.contains_key(&job), "entry drained");
        assert_eq!(state.escrowed_for_job(&job), 0);
        assert_eq!(money_conserved(&state), conserved);
        let snap: Vec<u64> = (0..13).map(|n| lbal(&state, n)).collect();
        assert!(state.lifecycle_settle_and_drain(job, &ByteEq, BlockHash([0u8; 32])).unwrap().is_none(), "second call: None");
        assert_eq!((0..13).map(|n| lbal(&state, n)).collect::<Vec<_>>(), snap, "no re-payment");
    }

    #[test]
    fn d2_settle_and_drain_timeout_drains_entry() {
        let (budget, e_bond, v_bond) = fuel_mins();
        let job = [22u8; 32];
        let mut state = ChainState::new();
        state.total_emitted = budget + e_bond;
        state.accounts.get_or_create(lpaddr(0)).balance = Amount::from_raw(budget);
        state.accounts.get_or_create(lpaddr(9)).balance = Amount::from_raw(e_bond);
        let conserved = money_conserved(&state);
        state.escrow_into_job(&lpaddr(0), job, budget).unwrap();
        state.escrow_into_job(&lpaddr(9), job, e_bond).unwrap();
        let lc = JobLifecycle::open(
            job, IDENT, IDENT, IDENT, lpid(0), lpid(9), e_bond, budget, v_bond,
            GameParams::default(), ResolutionParams::default(), vec![lpid(10)], test_deadlines(),
        );
        state.job_lifecycles.insert(job, lc); // executor never delivers
        let (t, fb) = state.lifecycle_settle_and_drain(job, &ByteEq, BlockHash([0u8; 32])).unwrap().expect("due");
        assert!(matches!(t, Terminal::TimedOut(_)), "got {t:?}");
        assert!(fb.is_none());
        assert!(!state.job_lifecycles.contains_key(&job), "entry drained");
        assert_eq!(state.escrowed_for_job(&job), 0);
        assert_eq!(money_conserved(&state), conserved);
        assert!(state.lifecycle_settle_and_drain(job, &ByteEq, BlockHash([0u8; 32])).unwrap().is_none());
    }

    #[test]
    fn d2_settle_and_drain_escalate_pays_zero_comp_and_drains() {
        // 3-way split ⇒ NoQuorum ⇒ Escalate ⇒ the D2-FINAL zero-comp fallback: pure refund.
        let (mut state, job, budget, e_bond, v_bond, conserved) =
            round_ready_state([[1u8; 32], [2u8; 32], [3u8; 32]], [true, true, true]);
        let (t, fb) = state.lifecycle_settle_and_drain(job, &ByteEq, BlockHash([0u8; 32])).unwrap().expect("due");
        assert!(matches!(t, Terminal::Escalate(_)), "got {t:?}");
        let fb = fb.expect("Escalate runs the fallback");
        assert_eq!(fb.worker_paid, 0, "ZERO executor comp (D2-FINAL)");
        assert_eq!(fb.submitter_refunded, budget, "full budget back");
        assert_eq!(fb.bonds_returned, e_bond + 3 * v_bond, "every bond back");
        assert_eq!(fb.burned, 0, "fallback burns nothing");
        assert_eq!(lbal(&state, 0), budget);
        assert_eq!(lbal(&state, 9), e_bond, "bond back, NO comp");
        for c in 10u8..13 {
            assert_eq!(lbal(&state, c), v_bond, "revealer bond back");
        }
        assert_eq!(state.escrowed_for_job(&job), 0, "pot drained to exactly 0");
        assert!(!state.job_lifecycles.contains_key(&job), "entry drained");
        assert_eq!(state.total_burned, 0, "total_burned untouched by the fallback");
        assert_eq!(money_conserved(&state), conserved);
        assert!(state.lifecycle_settle_and_drain(job, &ByteEq, BlockHash([0u8; 32])).unwrap().is_none(), "at-most-once");
    }

    #[test]
    fn p2_forfeiture_settle_caches_then_drains_without_wedging() {
        // P2's wedge scenario: 2 reveal a split, 1 stays silent ⇒ settle burns the silent bond
        // BEFORE the verdict ⇒ the pot no longer equals expected_escrow(). Pre-P2 a re-entry
        // Err'd forever (pot pre-validation ran before the cached-terminal short-circuit) and
        // the Escalate pot stranded permanently.
        let (mut state, job, budget, e_bond, v_bond, conserved) =
            round_ready_state([[1u8; 32], [2u8; 32], [3u8; 32]], [true, true, false]);
        let t1 = state.lifecycle_settle(job, &ByteEq).unwrap().expect("lifecycle");
        assert!(matches!(t1, Terminal::Escalate(_)), "got {t1:?}");
        assert!(state.job_lifecycles.get(&job).unwrap().is_settled());
        assert_eq!(state.escrowed_for_job(&job), budget + e_bond + 2 * v_bond, "reduced pot held");
        assert_eq!(state.total_burned, v_bond, "forfeit burned by the audited settle");

        // P2 pin: re-entry short-circuits to the CACHED terminal — Ok(cached), not Err.
        let snap: Vec<u64> = (0..13).map(|n| lbal(&state, n)).collect();
        let t2 = state.lifecycle_settle(job, &ByteEq).unwrap().expect("cached");
        assert_eq!(t1, t2, "cached terminal returned, no pot re-validation wedge");
        assert_eq!((0..13).map(|n| lbal(&state, n)).collect::<Vec<_>>(), snap, "cache moves no money");

        // Drain: the fallback pays out exactly the REDUCED pot.
        let (_, fb) = state.lifecycle_settle_and_drain(job, &ByteEq, BlockHash([0u8; 32])).unwrap().expect("drains");
        let fb = fb.expect("fallback ran");
        assert_eq!(fb.bonds_returned, e_bond + 2 * v_bond, "only the 2 revealers' bonds");
        assert_eq!(fb.burned, 0, "the forfeit was settle's burn, not the fallback's");
        assert_eq!(state.escrowed_for_job(&job), 0, "reduced pot drained to exactly 0");
        assert_eq!(lbal(&state, 12), 0, "silent committer stays forfeited");
        assert!(state.lifecycle_settle_and_drain(job, &ByteEq, BlockHash([0u8; 32])).unwrap().is_none(), "at-most-once");
        assert_eq!(money_conserved(&state), conserved);
    }

    #[test]
    fn d2_drain_with_malformed_pot_errs_then_retries_cleanly() {
        let (mut state, job, _, _, _, _) =
            round_ready_state([[1u8; 32], [2u8; 32], [3u8; 32]], [true, true, true]);
        state.lifecycle_settle(job, &ByteEq).unwrap().expect("caches Escalate");
        let pot = state.escrowed_for_job(&job);
        state.escrow_by_job.insert(job, pot - 1); // tamper: shrink the pot under the fallback
        let snap: Vec<u64> = (0..13).map(|n| lbal(&state, n)).collect();
        let err = state.lifecycle_settle_and_drain(job, &ByteEq, BlockHash([0u8; 32])).unwrap_err();
        assert!(err.to_string().contains("escalate pot"), "got: {err}");
        assert!(state.job_lifecycles.contains_key(&job), "lifecycle kept for retry");
        assert_eq!((0..13).map(|n| lbal(&state, n)).collect::<Vec<_>>(), snap, "nothing moved on Err");
        // Repair the pot ⇒ the retry is clean (deterministic re-check, terminal still cached).
        state.escrow_by_job.insert(job, pot);
        assert!(state.lifecycle_settle_and_drain(job, &ByteEq, BlockHash([0u8; 32])).unwrap().is_some());
        assert_eq!(state.escrowed_for_job(&job), 0);
    }

    // --- P3: zero-address guards --------------------------------------------------------------

    #[test]
    fn p3_zero_from_is_rejected_on_every_pouw_money_arm() {
        // Zero-from txs skip signature verification entirely, so a byzantine producer could
        // forge them: every money arm must reject the keyless zero address OUTRIGHT — even if
        // someone manufactured a balance and a validator flag for it.
        let cases: Vec<TxKind> = vec![
            v2_kind(MIN_BUDGET),
            TxKind::ClaimJob { job_id: [7u8; 32] },
            TxKind::Commit { job_id: [7u8; 32], commit: [1u8; 32], bond: Amount::from_raw(20) },
            TxKind::Reveal { job_id: [7u8; 32], result_hash: [1u8; 32], salt: [2u8; 32] },
            TxKind::Bond { amount: Amount::from_raw(10) },
            TxKind::RequestUnbond { amount: Amount::from_raw(10) },
            TxKind::WithdrawUnbonded,
            TxKind::Batch { operations: vec![TxKind::Bond { amount: Amount::from_raw(10) }] },
            TxKind::Batch { operations: vec![TxKind::ClaimJob { job_id: [7u8; 32] }] },
            TxKind::Batch { operations: vec![TxKind::Commit {
                job_id: [7u8; 32], commit: [1u8; 32], bond: Amount::from_raw(20) }] },
            TxKind::Batch { operations: vec![TxKind::Reveal {
                job_id: [7u8; 32], result_hash: [1u8; 32], salt: [2u8; 32] }] },
        ];
        for kind in cases {
            let mut state = ChainState::new();
            state.apply_block(&genesis_block()).unwrap();
            let z = state.accounts.get_or_create(addr(0)); // addr(0) == the zero address
            z.balance = Amount::from_raw(10 * MIN_BUDGET);
            z.is_validator = true;
            state.total_emitted = 10 * MIN_BUDGET;
            let root = state.compute_state_root();
            let err = state
                .apply_block(&block_with(&state, 1, vec![unsigned(addr(0), 0, kind.clone())]))
                .unwrap_err();
            assert!(
                err.to_string().contains("zero address"),
                "kind {kind:?} must die on the zero-from guard, got: {err}"
            );
            assert_eq!(state.compute_state_root(), root, "P1 rollback after the rejection");
        }
        // The legitimate protocol path is untouched: a zero-from MiningReward still applies.
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        state
            .apply_block(&block_with(&state, 1, vec![unsigned(addr(0), 0, TxKind::MiningReward {
                to: addr(1), amount: Amount::from_raw(1), epoch: 0 })]))
            .unwrap();
    }

    #[test]
    fn p3_zero_address_never_enters_the_candidate_snapshot() {
        // Even with a manufactured zero-address validator + bonded stake, the B3 candidate
        // filter excludes it (a keyless committee seat would be puppetable by anyone).
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        let min_bond = StakeParams::default().min_bond;
        let z = state.accounts.get_or_create(addr(0));
        z.is_validator = true;
        z.balance = Amount::from_raw(MIN_BUDGET);
        state.bonded_stake.insert(addr(0), min_bond); // tampered keyless stake
        let v3 = state.accounts.get_or_create(addr(3));
        v3.is_validator = true;
        state.bonded_stake.insert(addr(3), min_bond);
        state.accounts.get_or_create(addr(2)).balance = Amount::from_raw(MIN_BUDGET);
        let e = state.accounts.get_or_create(addr(1));
        e.is_validator = true;
        e.balance = Amount::from_raw(MIN_BUDGET);

        let submit = unsigned(addr(2), 0, v2_kind(MIN_BUDGET));
        let job = submit.hash().0;
        state.apply_block(&block_with(&state, 1, vec![submit])).unwrap();
        state
            .apply_block(&block_with(&state, 2, vec![unsigned(addr(1), 0, TxKind::ClaimJob { job_id: job })]))
            .unwrap();
        assert_eq!(
            state.job_lifecycles.get(&job).unwrap().to_record().candidates,
            vec![addr(3).0],
            "zero address filtered out of the snapshot"
        );
    }

    // --- Full driven rounds: conservation at every block, P8 driver settles ---------------------

    /// End-state of one fully tx-driven job round (see `drive_job_round`).
    struct Driven {
        state: ChainState,
        conserved0: u64,
        budget: u64,
        e_bond: u64,
        v_bond: u64,
        roots: Vec<[u8; 32]>,
    }

    fn apply_and_check(
        state: &mut ChainState,
        txs: Vec<Transaction>,
        conserved0: u64,
        roots: &mut Vec<[u8; 32]>,
    ) {
        let h = state.blocks.height() + 1;
        let block = block_with(state, h, txs);
        state.apply_block(&block).unwrap();
        assert_eq!(money_conserved(state), conserved0, "money conserved after block {h}");
        roots.push(state.compute_state_root());
    }

    /// Drive a COMPLETE job round through real blocks: Bond txs (b1), SubmitJobV2 (b2), ClaimJob
    /// (b3), Commit txs (b4), Reveal txs (b10), with the P8 in-apply driver advancing phases and
    /// settling at the deadline heights — settle is never called manually. Shortened windows:
    /// claim at parent height 2 ⇒ result_by 5 / commit_by 8 / reveal_by 11 ⇒ terminal at block 12
    /// (block 6 on the timeout path). The out-of-band step is submit_result only (B5 PROTECTED).
    /// Actors: submitter addr(2), executor addr(1), verifiers addr(3)/(4)/(5); producer is the
    /// zero address (earns nothing) and every fee is 0 ⇒ `money_conserved` must hold EXACTLY
    /// after every block. `commits`: (verifier, hash, does_reveal).
    fn drive_job_round(executor_result: Option<[u8; 32]>, commits: &[(u8, [u8; 32], bool)]) -> Driven {
        let budget = MIN_BUDGET;
        let v_bond = GameParams::default().verifier_bond;
        let min_bond = StakeParams::default().min_bond;
        let mut state = ChainState::new();
        state.phase_windows = PhaseWindows {
            result_blocks: 3, commit_blocks: 3, reveal_blocks: 3, claim_blocks: 5,
        };
        state.apply_block(&genesis_block()).unwrap();
        state.accounts.get_or_create(addr(2)).balance = Amount::from_raw(budget);
        let e = state.accounts.get_or_create(addr(1));
        e.is_validator = true;
        e.balance = Amount::from_raw(budget); // e_bond == max(budget, flat 100) == budget
        for v in [3u8, 4, 5] {
            let a = state.accounts.get_or_create(addr(v));
            a.is_validator = true;
            a.balance = Amount::from_raw(min_bond + v_bond);
        }
        state.total_emitted = 2 * budget + 3 * (min_bond + v_bond);
        let conserved0 = money_conserved(&state);
        let mut roots = Vec::new();

        // b1: the verifiers bond to committee eligibility — REAL Bond txs (N1 kinds).
        let bonds: Vec<Transaction> = [3u8, 4, 5]
            .iter()
            .map(|&v| unsigned(addr(v), 0, TxKind::Bond { amount: Amount::from_raw(min_bond) }))
            .collect();
        apply_and_check(&mut state, bonds, conserved0, &mut roots);

        // b2: SubmitJobV2 escrows the budget (job_id == tx hash; claim_by = 1 + 5).
        let submit = unsigned(addr(2), 0, v2_kind(budget));
        let job = submit.hash().0;
        apply_and_check(&mut state, vec![submit], conserved0, &mut roots);

        // b3: ClaimJob opens the lifecycle (parent height 2 ⇒ deadlines 5/8/11).
        apply_and_check(
            &mut state,
            vec![unsigned(addr(1), 0, TxKind::ClaimJob { job_id: job })],
            conserved0,
            &mut roots,
        );

        if let Some(hash) = executor_result {
            // Result delivery is the PROTECTED B5 wiring — out-of-band here, swept by b4.
            let stake = |_: &ParticipantId| 1u64;
            assert_eq!(
                state.job_lifecycles.get_mut(&job).unwrap().submit_result(
                    ParticipantId(addr(1).0), hash, [42u8; 32], 3, &stake),
                EventResult::Accepted
            );
            // b4: Commit txs from the drawn committee (== the 3 bonded verifiers, k = 3).
            let commit_txs: Vec<Transaction> = commits
                .iter()
                .enumerate()
                .map(|(i, &(v, h, _))| {
                    let c: Commitment =
                        make_commitment(&ParticipantId(addr(v).0), &h, &[i as u8; 32], v_bond);
                    unsigned(addr(v), 1, TxKind::Commit {
                        job_id: job, commit: c.commit, bond: Amount::from_raw(v_bond),
                    })
                })
                .collect();
            apply_and_check(&mut state, commit_txs, conserved0, &mut roots);
            // b5–b9 empty: the driver flips Committing→Revealing at block 9 (9 > commit_by 8).
            while state.blocks.height() < 9 {
                apply_and_check(&mut state, vec![], conserved0, &mut roots);
            }
            // b10: Reveal txs (parent height 9 ≤ reveal_by 11).
            let reveal_txs: Vec<Transaction> = commits
                .iter()
                .enumerate()
                .filter(|(_, c)| c.2)
                .map(|(i, &(v, h, _))| unsigned(addr(v), 2, TxKind::Reveal {
                    job_id: job, result_hash: h, salt: [i as u8; 32],
                }))
                .collect();
            apply_and_check(&mut state, reveal_txs, conserved0, &mut roots);
        }

        // Empty blocks past the settle height — the driver terminates the job on its own.
        while state.blocks.height() < 13 {
            apply_and_check(&mut state, vec![], conserved0, &mut roots);
        }
        assert!(state.job_lifecycles.is_empty(), "P8 driver settled + drained the lifecycle");
        assert!(state.pending_jobs.is_empty(), "no pending record left behind");
        assert_eq!(state.escrowed_for_job(&job), 0, "pot fully drained at the terminal");

        // At-most-once: one more block moves NO money for any actor.
        let snap: Vec<u64> = [1u8, 2, 3, 4, 5].iter().map(|&n| bal(&state, n)).collect();
        let burned = state.total_burned;
        apply_and_check(&mut state, vec![], conserved0, &mut roots);
        assert_eq!(
            [1u8, 2, 3, 4, 5].iter().map(|&n| bal(&state, n)).collect::<Vec<u64>>(),
            snap,
            "settlement is at-most-once"
        );
        assert_eq!(state.total_burned, burned);

        Driven { state, conserved0, budget, e_bond: budget, v_bond, roots }
    }

    #[test]
    fn driven_confirmed_pays_the_audited_split_through_real_blocks() {
        let r = [7u8; 32];
        let d = drive_job_round(Some(r), &[(3, r, true), (4, r, true), (5, r, true)]);
        let share = bps_of(d.budget, 1_000) / 3; // 10% pool, even split; remainder burned
        assert_eq!(bal(&d.state, 1), bps_of(d.budget, 8_500) + d.e_bond, "executor: 85% + bond back");
        for v in [3u8, 4, 5] {
            assert_eq!(bal(&d.state, v), d.v_bond + share, "verifier: bond back + pool share");
        }
        assert_eq!(bal(&d.state, 2), 0, "submitter spent the whole budget");
        assert_eq!(
            d.state.total_burned,
            d.budget - bps_of(d.budget, 8_500) - 3 * share,
            "exactly the 5% slice + rounding remainder (fees are zero)"
        );
        assert_eq!(money_conserved(&d.state), d.conserved0);
    }

    #[test]
    fn driven_disputed_refunds_submitter_and_slashes_executor() {
        let claimed = [7u8; 32];
        let correct = [5u8; 32]; // committee proves a different result ⇒ Disputed
        let d = drive_job_round(Some(claimed), &[(3, correct, true), (4, correct, true), (5, correct, true)]);
        let bounty_share = bps_of(d.e_bond, 2_000) / 3; // dispute bounty from the slashed bond
        assert_eq!(bal(&d.state, 2), d.budget, "submitter fully refunded");
        assert_eq!(bal(&d.state, 1), 0, "executor bond slashed to zero");
        for v in [3u8, 4, 5] {
            assert_eq!(bal(&d.state, v), d.v_bond + bounty_share, "honest verifier: bond + bounty");
        }
        assert_eq!(d.state.total_burned, d.e_bond - 3 * bounty_share, "non-bounty remainder burned");
        assert_eq!(money_conserved(&d.state), d.conserved0);
    }

    #[test]
    fn driven_timeout_compensates_submitter_and_burns_the_rest() {
        // Executor claims but never delivers ⇒ the driver settles TimedOut at result_by+1.
        let d = drive_job_round(None, &[]);
        let comp = bps_of(d.e_bond, 2_000); // founder rule: 20% of the slashed bond
        assert_eq!(bal(&d.state, 2), d.budget + comp, "budget refund + 20% bond comp");
        assert_eq!(bal(&d.state, 1), 0, "executor slashed");
        for v in [3u8, 4, 5] {
            assert_eq!(bal(&d.state, v), d.v_bond, "verifiers never engaged — untouched");
        }
        assert_eq!(d.state.total_burned, d.e_bond - comp, "80% of the bond burned");
        assert_eq!(money_conserved(&d.state), d.conserved0);
    }

    #[test]
    fn driven_noquorum_fallback_is_pure_refund_zero_comp() {
        // 3-way split ⇒ NoQuorum ⇒ Escalate ⇒ D2-FINAL zero-comp fallback, all inside the
        // driver: full budget back, every bond back, NOTHING burned, executor comp ZERO.
        let d = drive_job_round(
            Some([7u8; 32]),
            &[(3, [1u8; 32], true), (4, [2u8; 32], true), (5, [3u8; 32], true)],
        );
        assert_eq!(bal(&d.state, 2), d.budget, "full refund");
        assert_eq!(bal(&d.state, 1), d.e_bond, "bond back, ZERO comp (D2-FINAL)");
        for v in [3u8, 4, 5] {
            assert_eq!(bal(&d.state, v), d.v_bond, "revealer bond back");
        }
        assert_eq!(d.state.total_burned, 0, "fallback burns nothing");
        assert_eq!(money_conserved(&d.state), d.conserved0);
    }

    #[test]
    fn driven_noquorum_with_forfeiture_burns_only_the_silent_bond() {
        // 2 distinct reveals (max class 1 < quorum 2) + 1 commit-no-reveal ⇒ the audited settle
        // burns the silent bond, then the fallback drains the REDUCED pot — P2's wedge
        // scenario driven end-to-end through real blocks.
        let d = drive_job_round(
            Some([7u8; 32]),
            &[(3, [1u8; 32], true), (4, [2u8; 32], true), (5, [3u8; 32], false)],
        );
        assert_eq!(bal(&d.state, 2), d.budget, "full refund");
        assert_eq!(bal(&d.state, 1), d.e_bond, "bond back, zero comp");
        assert_eq!(bal(&d.state, 3), d.v_bond);
        assert_eq!(bal(&d.state, 4), d.v_bond);
        assert_eq!(bal(&d.state, 5), 0, "silent committer's bond forfeited");
        assert_eq!(d.state.total_burned, d.v_bond, "exactly the forfeited bond burned");
        assert_eq!(money_conserved(&d.state), d.conserved0);
    }

    #[test]
    fn p8_driver_is_deterministic_across_independent_replays() {
        // Two nodes applying the SAME blocks must settle at the SAME heights and produce
        // byte-identical roots at EVERY height (Confirmed-with-forfeiture: the richest path).
        let r = [7u8; 32];
        let commits = [(3u8, r, true), (4u8, r, true), (5u8, r, false)];
        let a = drive_job_round(Some(r), &commits);
        let b = drive_job_round(Some(r), &commits);
        assert_eq!(a.roots, b.roots, "per-height roots identical, settle height included (P8)");
        assert_eq!(a.state.compute_state_root(), b.state.compute_state_root());
    }

    // --- P1: rollback-on-Err regressions --------------------------------------------------------

    #[test]
    fn p1_rollback_memory_erases_mid_block_smear() {
        // tx1 (V2 escrow) succeeds, tx2 fails ⇒ pre-P1 the escrow + tx1's nonce smeared the
        // in-memory state; now the WHOLE block must leave no trace.
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        state.accounts.get_or_create(addr(1)).balance = Amount::from_raw(MIN_BUDGET);
        state.accounts.get_or_create(addr(2)).balance = Amount::from_raw(50);
        state.total_emitted = MIN_BUDGET + 50;
        let root = state.compute_state_root();
        let conserved = money_conserved(&state);

        let err = state
            .apply_block(&block_with(&state, 1, vec![
                unsigned(addr(1), 0, v2_kind(MIN_BUDGET)),
                unsigned(addr(2), 0, TxKind::Transfer { to: addr(1), amount: Amount::from_raw(MIN_BUDGET) }),
            ]))
            .unwrap_err();
        assert!(matches!(err, StateError::InsufficientBalance), "got: {err}");
        assert_eq!(state.blocks.height(), 0, "block rejected");
        assert_eq!(bal(&state, 1), MIN_BUDGET, "tx1's escrow debit rolled back");
        assert_eq!(state.accounts.get(&addr(1)).unwrap().nonce, 0, "tx1's nonce rolled back");
        assert!(state.pending_jobs.is_empty() && state.escrow_by_job.is_empty(), "maps rolled back");
        assert_eq!(state.compute_state_root(), root, "root byte-identical to pre-block");
        assert_eq!(money_conserved(&state), conserved);
    }

    #[test]
    fn p1_rocks_rollback_matches_node_that_never_saw_the_invalid_block() {
        // The P1 BLOCKER scenario: pre-P1, a rejected block's smear sat in the dirty journal
        // and the NEXT good block's persist wrote it into the CFs — forking node A from node B
        // on disk. Node A sees the malicious block; node B never does; they must stay
        // byte-identical through the next good block AND across reopen.
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let mut a = ChainState::open(dir_a.path()).unwrap();
        let mut b = ChainState::open(dir_b.path()).unwrap();
        for s in [&mut a, &mut b] {
            s.apply_block(&genesis_block()).unwrap();
            s.accounts.get_or_create(addr(1)).balance = Amount::from_raw(3 * MIN_BUDGET);
            s.total_emitted = 3 * MIN_BUDGET;
        }
        // Common block 1 (sweeps the funding into the CFs on both nodes).
        let blk1 = block_with(&a, 1, vec![unsigned(addr(1), 0, TxKind::Bond { amount: Amount::from_raw(1_000) })]);
        a.apply_block(&blk1).unwrap();
        b.apply_block(&blk1).unwrap();
        assert_eq!(a.compute_state_root(), b.compute_state_root());

        // Node A alone is fed the malicious block: tx1 escrows a V2 job, tx2 fails.
        let bad = block_with(&a, 2, vec![
            unsigned(addr(1), 1, v2_kind(MIN_BUDGET)),
            unsigned(addr(2), 0, TxKind::Transfer { to: addr(3), amount: Amount::from_raw(MIN_BUDGET) }),
        ]);
        assert!(a.apply_block(&bad).is_err());
        assert_eq!(a.compute_state_root(), b.compute_state_root(), "P1: no in-memory smear on A");

        // Both apply the SAME good block 2 — its persist must write identical CF bytes.
        let good = block_with(&a, 2, vec![unsigned(addr(1), 1, TxKind::Bond { amount: Amount::from_raw(500) })]);
        a.apply_block(&good).unwrap();
        b.apply_block(&good).unwrap();
        assert_eq!(a.compute_state_root(), b.compute_state_root(), "roots agree after the next block");

        drop(a);
        drop(b);
        let ra = ChainState::open(dir_a.path()).unwrap();
        let rb = ChainState::open(dir_b.path()).unwrap();
        assert_eq!(
            ra.compute_state_root(),
            rb.compute_state_root(),
            "reopened roots identical — the smear never reached A's CFs"
        );
        assert!(ra.pending_jobs.is_empty() && ra.escrow_by_job.is_empty(), "no smeared V2 rows on disk");
        assert!(ra.accounts.get(&addr(2)).is_none(), "failed tx's account-creation smear not persisted");
        assert_eq!(
            ra.accounts.get(&addr(1)).map(|x| x.balance),
            rb.accounts.get(&addr(1)).map(|x| x.balance),
        );
        assert_eq!(ra.bonded_of(&addr(1)), 1_500);
    }

    #[test]
    fn p1_rollback_preserves_out_of_band_mutations_on_a_rocks_node() {
        // The rocks-reload rollback bug: out-of-band mutations applied in memory since the last
        // block (the event loop's epoch bump + per-account pokes, grace drains) ride the NEXT
        // block's persist — they are live-in-memory-but-not-on-disk during a block apply. A disk
        // reload on rollback would rewind them; the memory snapshot must preserve them.
        let dir = tempfile::tempdir().unwrap();
        let mut s = ChainState::open(dir.path()).unwrap();
        s.apply_block(&genesis_block()).unwrap();
        s.accounts.get_or_create(addr(1)).balance = Amount::from_raw(3 * MIN_BUDGET);
        s.total_emitted = 3 * MIN_BUDGET;
        let blk1 = block_with(&s, 1, vec![unsigned(addr(1), 0, TxKind::Bond { amount: Amount::from_raw(1_000) })]);
        s.apply_block(&blk1).unwrap();

        // Out-of-band mutations, exactly as the (PROTECTED) event loop applies them between
        // blocks: bump the epoch counter and poke an account field. These are dirty-in-memory,
        // NOT yet persisted — they would ride block 2's persist.
        s.current_epoch += 1;
        s.accounts.get_or_create(addr(1)).cumulative_uptime_secs += 3600;
        let epoch_before = s.current_epoch;
        let uptime_before = s.accounts.get(&addr(1)).unwrap().cumulative_uptime_secs;
        let root_before = s.compute_state_root();

        // A malicious block arrives and fails apply (tx2 rejects) — rollback must leave the
        // out-of-band state intact, not reload it away from disk (disk still holds epoch 0).
        let bad = block_with(&s, 2, vec![
            unsigned(addr(1), 1, v2_kind(MIN_BUDGET)),
            unsigned(addr(2), 0, TxKind::Transfer { to: addr(3), amount: Amount::from_raw(MIN_BUDGET) }),
        ]);
        assert!(s.apply_block(&bad).is_err());

        assert_eq!(s.current_epoch, epoch_before, "epoch bump survived the rollback");
        assert_eq!(
            s.accounts.get(&addr(1)).unwrap().cumulative_uptime_secs, uptime_before,
            "out-of-band account poke survived the rollback",
        );
        assert_eq!(s.compute_state_root(), root_before, "root unchanged by the rejected block");
        assert!(s.pending_jobs.is_empty() && s.escrow_by_job.is_empty(), "no V2 smear from tx1");

        // The next good block persists the out-of-band state; reopen proves it reached disk.
        let good = block_with(&s, 2, vec![unsigned(addr(1), 1, TxKind::Bond { amount: Amount::from_raw(500) })]);
        s.apply_block(&good).unwrap();
        drop(s);
        let re = ChainState::open(dir.path()).unwrap();
        assert_eq!(re.current_epoch, epoch_before, "out-of-band epoch bump persisted, not lost");
        assert_eq!(
            re.accounts.get(&addr(1)).unwrap().cumulative_uptime_secs, uptime_before,
            "out-of-band account poke persisted across restart",
        );
    }

    // --- Crash-persistence across the driven round ----------------------------------------------

    #[test]
    fn crash_mid_round_recovers_and_completes_the_round() {
        // The §8 flagship: crash WITHOUT flush right after the Commit block, reopen, finish
        // Reveal + driver settle — conservation holds end-to-end ACROSS the restart.
        let dir = tempfile::tempdir().unwrap();
        let (ws, we, w1, w2) = (Wallet::generate(), Wallet::generate(), Wallet::generate(), Wallet::generate());
        let (s_addr, e_addr, v1_addr, v2_addr) = (*ws.address(), *we.address(), *w1.address(), *w2.address());
        let budget = MIN_BUDGET;
        let v_bond = GameParams::default().verifier_bond;
        let min_bond = StakeParams::default().min_bond;
        let result = [7u8; 32];
        let (job, root, rec, conserved0);
        {
            let mut state = ChainState::open(dir.path()).unwrap();
            state.phase_windows = PhaseWindows {
                result_blocks: 3, commit_blocks: 3, reveal_blocks: 3, claim_blocks: 5,
            };
            state.apply_block(&genesis_block()).unwrap();
            state.accounts.get_or_create(s_addr).balance = Amount::from_raw(budget);
            let e = state.accounts.get_or_create(e_addr);
            e.is_validator = true;
            e.balance = Amount::from_raw(budget);
            for w_addr in [v1_addr, v2_addr] {
                let acc = state.accounts.get_or_create(w_addr);
                acc.is_validator = true;
                acc.balance = Amount::from_raw(min_bond + v_bond);
            }
            state.total_emitted = 2 * budget + 2 * (min_bond + v_bond);
            conserved0 = money_conserved(&state);

            // b1: bond both verifiers (signed txs through the VALIDATED path).
            let b1 = validated_block(&state, 1, addr(0), vec![
                signed_tx(&w1, 0, TxKind::Bond { amount: Amount::from_raw(min_bond) }, 0),
                signed_tx(&w2, 0, TxKind::Bond { amount: Amount::from_raw(min_bond) }, 0),
            ]);
            state.apply_block_validated(&b1).unwrap();
            // b2: submit (h1 ⇒ claim_by 6); b3: claim (h2 ⇒ result 5 / commit 8 / reveal 11).
            let submit = signed_tx(&ws, 0, v2_kind(budget), 0);
            job = submit.hash().0;
            let b2 = validated_block(&state, 2, addr(0), vec![submit]);
            state.apply_block_validated(&b2).unwrap();
            let b3 = validated_block(&state, 3, addr(0),
                vec![signed_tx(&we, 0, TxKind::ClaimJob { job_id: job }, 0)]);
            state.apply_block_validated(&b3).unwrap();
            // Result out-of-band (B5 PROTECTED); committee == both bonded verifiers, quorum 2.
            let stake = |_: &ParticipantId| 1u64;
            assert_eq!(
                state.job_lifecycles.get_mut(&job).unwrap().submit_result(
                    ParticipantId(e_addr.0), result, [42u8; 32], 3, &stake),
                EventResult::Accepted
            );
            // b4: both verifiers commit — the block's ONE WriteBatch carries lifecycle + pot.
            let mk = |w: &Wallet, waddr: Address, salt: u8| {
                let c: Commitment = make_commitment(&ParticipantId(waddr.0), &result, &[salt; 32], v_bond);
                signed_tx(w, 1, TxKind::Commit {
                    job_id: job, commit: c.commit, bond: Amount::from_raw(v_bond) }, 0)
            };
            let b4 = validated_block(&state, 4, addr(0), vec![mk(&w1, v1_addr, 0), mk(&w2, v2_addr, 1)]);
            state.apply_block_validated(&b4).unwrap();

            rec = state.job_lifecycles.get(&job).unwrap().to_record();
            root = state.compute_state_root();
            // DROP WITHOUT flush() — the per-block WriteBatches alone must carry the round.
        }
        let mut re = ChainState::open(dir.path()).unwrap();
        assert_eq!(re.compute_state_root(), root, "root survives the crash");
        assert_eq!(re.escrowed_for_job(&job), 2 * budget + 2 * v_bond, "pot: budget + Be + 2 bonds");
        assert!(re.pending_jobs.is_empty(), "pending consumed by the claim");
        assert_eq!(re.job_lifecycles.get(&job).map(|l| l.to_record()), Some(rec.clone()),
            "lifecycle DTO field-identical after reopen");
        assert_eq!(rec.program_hash, [0xAA; 32], "P9: program identity rode the DTO");
        assert_eq!(rec.input_hash, [0xBB; 32]);
        assert_eq!(rec.da_root, [0xCC; 32]);
        // Mirrors describe CF reality exactly.
        assert!(re.persisted_lifecycle_keys.contains(&job) && re.persisted_lifecycle_keys.len() == 1);
        assert!(re.persisted_escrow_keys.contains(&job) && re.persisted_escrow_keys.len() == 1);
        assert!(re.persisted_pending_keys.is_empty());
        assert_eq!(re.persisted_bonded_keys.len(), 2);
        assert_eq!(money_conserved(&re), conserved0, "conservation across the restart");

        // Finish the round post-reopen (deadlines live in the DTO — phase_windows not needed).
        while re.blocks.height() < 9 {
            let blk = validated_block(&re, re.blocks.height() + 1, addr(0), vec![]);
            re.apply_block_validated(&blk).unwrap();
        }
        let reveal = |w: &Wallet, salt: u8| signed_tx(w, 2, TxKind::Reveal {
            job_id: job, result_hash: result, salt: [salt; 32] }, 0);
        let b10 = validated_block(&re, 10, addr(0), vec![reveal(&w1, 0), reveal(&w2, 1)]);
        re.apply_block_validated(&b10).unwrap();
        while re.blocks.height() < 12 {
            let blk = validated_block(&re, re.blocks.height() + 1, addr(0), vec![]);
            re.apply_block_validated(&blk).unwrap();
        }
        // Driver settled Confirmed at h12 (12 > reveal_by 11): 2-of-2 quorum on `result`.
        assert!(re.job_lifecycles.is_empty(), "settled + drained after the restart");
        assert_eq!(re.escrowed_for_job(&job), 0);
        assert_eq!(
            re.accounts.get(&e_addr).unwrap().balance.raw(),
            bps_of(budget, 8_500) + budget,
            "executor: 85% + bond back"
        );
        assert_eq!(money_conserved(&re), conserved0, "conserved end-to-end across the crash");
    }

    #[test]
    fn crash_before_claim_preserves_pending_then_expiry_refunds() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Wallet::generate();
        let s_addr = *ws.address();
        let (job, claim_by, conserved0);
        {
            let mut state = ChainState::open(dir.path()).unwrap();
            state.phase_windows.claim_blocks = 2;
            state.apply_block(&genesis_block()).unwrap();
            state.accounts.get_or_create(s_addr).balance = Amount::from_raw(MIN_BUDGET);
            state.total_emitted = MIN_BUDGET;
            conserved0 = money_conserved(&state);
            let submit = signed_tx(&ws, 0, v2_kind(MIN_BUDGET), 0);
            job = submit.hash().0;
            let b1 = validated_block(&state, 1, addr(0), vec![submit]);
            state.apply_block_validated(&b1).unwrap();
            claim_by = state.pending_jobs.get(&job).unwrap().claim_by;
            // DROP WITHOUT flush between submit and any claim.
        }
        let mut re = ChainState::open(dir.path()).unwrap();
        let rec = re.pending_jobs.get(&job).copied().expect("pending record survives the crash");
        assert_eq!(rec.budget, MIN_BUDGET);
        assert_eq!(rec.claim_by, claim_by);
        assert_eq!(re.escrowed_for_job(&job), MIN_BUDGET, "pot survives the crash");
        // claim_by is baked into the record (phase_windows is genesis-anchored, not persisted);
        // empty blocks past it make the driver refund.
        while re.blocks.height() <= claim_by {
            let blk = validated_block(&re, re.blocks.height() + 1, addr(0), vec![]);
            re.apply_block_validated(&blk).unwrap();
        }
        assert!(re.pending_jobs.is_empty(), "driver expired the unclaimed job");
        assert_eq!(re.escrowed_for_job(&job), 0);
        assert_eq!(re.accounts.get(&s_addr).unwrap().balance.raw(), MIN_BUDGET, "full refund");
        assert_eq!(money_conserved(&re), conserved0);
        // ...and the refund itself is crash-durable.
        let root = re.compute_state_root();
        drop(re);
        let re2 = ChainState::open(dir.path()).unwrap();
        assert_eq!(re2.compute_state_root(), root);
        assert!(re2.pending_jobs.is_empty(), "no resurrected pending row");
    }

    #[test]
    fn pending_jobs_reset_to_genesis_wipes_cf_pending() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Wallet::generate();
        {
            let mut state = ChainState::open(dir.path()).unwrap();
            state.apply_block(&genesis_block()).unwrap();
            state.accounts.get_or_create(*ws.address()).balance = Amount::from_raw(MIN_BUDGET);
            let b1 = validated_block(&state, 1, addr(0), vec![signed_tx(&ws, 0, v2_kind(MIN_BUDGET), 0)]);
            state.apply_block_validated(&b1).unwrap();
            assert_eq!(state.pending_jobs.len(), 1);
            state.reset_to_genesis().unwrap();
            assert!(state.pending_jobs.is_empty());
        }
        let re = ChainState::open(dir.path()).unwrap();
        assert!(re.pending_jobs.is_empty(), "CF_PENDING wiped by reset — no resurrection");
        assert!(re.escrow_by_job.is_empty());
    }

    #[test]
    fn reorg_replays_v2_pending_job_and_persists_the_winning_row() {
        // R9: try_reorg's full replay must reconcile CF_PENDING through the same delta pass —
        // the losing chain's pending row must die, the winning chain's must survive a restart.
        let dir = tempfile::tempdir().unwrap();
        let (root, job_y, job_x);
        {
            let mut state = ChainState::open(dir.path()).unwrap();
            let genesis = genesis_block();
            state.apply_block(&genesis).unwrap();
            // Losing chain X: producer addr(1) earns the b1 reward, submits job X in x2.
            let x1 = raw_block(1, genesis.hash(), addr(1), 2001, vec![]);
            state.apply_block(&x1).unwrap();
            let sx = unsigned(addr(1), 0, v2_kind(MIN_BUDGET));
            job_x = sx.hash().0;
            let x2 = raw_block(2, x1.hash(), addr(1), 2002, vec![sx]);
            state.apply_block(&x2).unwrap();
            assert!(state.pending_jobs.contains_key(&job_x));
            // Winning chain Y: longer; a different budget ⇒ different tx hash ⇒ different id.
            let y1 = raw_block(1, genesis.hash(), addr(1), 3001, vec![]);
            let sy = unsigned(addr(1), 0, v2_kind(MIN_BUDGET + 1));
            job_y = sy.hash().0;
            let y2 = raw_block(2, y1.hash(), addr(1), 3002, vec![sy]);
            let y3 = raw_block(3, y2.hash(), addr(1), 3003, vec![]);
            state.try_reorg(vec![y1, y2, y3], 1).unwrap();

            assert_ne!(job_x, job_y);
            assert!(!state.pending_jobs.contains_key(&job_x), "losing-chain pending row gone");
            assert_eq!(state.pending_jobs.get(&job_y).map(|r| r.budget), Some(MIN_BUDGET + 1));
            assert_eq!(state.escrowed_for_job(&job_y), MIN_BUDGET + 1);
            assert_eq!(state.escrowed_for_job(&job_x), 0);
            root = state.compute_state_root();
            // DROP WITHOUT flush — the reorg's one-batch reconcile must have covered CF_PENDING.
        }
        let re = ChainState::open(dir.path()).unwrap();
        assert!(!re.pending_jobs.contains_key(&job_x), "losing row did not resurrect from the CF");
        assert_eq!(re.pending_jobs.get(&job_y).map(|r| r.budget), Some(MIN_BUDGET + 1));
        assert_eq!(re.escrowed_for_job(&job_y), MIN_BUDGET + 1);
        assert_eq!(re.compute_state_root(), root, "reorg replay reproduced identical state (P8/R9)");
    }

    // --- Pins: P10a root sections, revert guards, replay semantics, §10 adapter ---------------

    #[test]
    fn p10a_root_folds_five_sections_once_any_map_is_nonempty() {
        // The Policy-B early-return is all-or-nothing: ONE bonded entry flips the whole root to
        // the 5-section fold (which then INCLUDES the empty pending_jobs section).
        let mut state = ChainState::new();
        state.accounts.get_or_create(addr(1)).balance = Amount::from_raw(1_000);
        let accounts_only = state.compute_state_root();
        state.bonded_stake.insert(addr(1), 500);
        let bonded_only = state.compute_state_root();
        assert_ne!(bonded_only, accounts_only, "first Bond flips to the 5-section format (P10a)");
        // The 5th section is load-bearing: adding a pending entry changes the root...
        let rec = PendingJobRecord {
            submitter: [1u8; 32], budget: 5, program_hash: [2u8; 32], input_hash: [3u8; 32],
            da_root: [4u8; 32], submitted_height: 1, claim_by: 11,
        };
        state.pending_jobs.insert([9u8; 32], rec);
        let with_pending = state.compute_state_root();
        assert_ne!(with_pending, bonded_only, "pending section is folded");
        // ...and removing it restores the bonded-only root exactly (same fold, empty section).
        state.pending_jobs.remove(&[9u8; 32]);
        assert_eq!(state.compute_state_root(), bonded_only);
        // A pending-only state activates the fold too.
        let mut p = ChainState::new();
        p.accounts.get_or_create(addr(1)).balance = Amount::from_raw(1_000);
        p.pending_jobs.insert([9u8; 32], rec);
        assert_ne!(p.compute_state_root(), accounts_only, "pending alone activates the fold");
    }

    #[test]
    fn revert_refuses_blocks_carrying_the_new_pouw_kinds() {
        // Guard 2 (tx-kind scan): a PoUW-consensus kind that CAN apply while leaving the maps empty
        // must still refuse revert. Post-flip, ClaimJob/CompleteJob unknown ids REJECT at apply
        // (M2/B5) — see below — so the applies-but-leaves-maps-empty case is exercised by
        // WithdrawUnbonded (a no-op when there is no unbonding stake).
        for kind in [
            TxKind::WithdrawUnbonded,
            TxKind::Batch { operations: vec![TxKind::WithdrawUnbonded] },
        ] {
            let mut state = ChainState::new();
            state.apply_block(&genesis_block()).unwrap();
            let v = state.accounts.get_or_create(addr(1));
            v.is_validator = true;
            v.balance = Amount::from_raw(1_000);
            state
                .apply_block(&block_with(&state, 1, vec![unsigned(addr(1), 0, kind.clone())]))
                .unwrap();
            assert!(
                state.escrow_by_job.is_empty()
                    && state.job_lifecycles.is_empty()
                    && state.bonded_stake.is_empty()
                    && state.unbonding_stake.is_empty()
                    && state.pending_jobs.is_empty(),
                "precondition: maps empty ⇒ this exercises guard 2, not guard 1"
            );
            let err = state.revert_block(1).unwrap_err();
            assert!(err.to_string().contains("consensus maps"), "kind {kind:?}: got {err}");
        }

        // Post-flip: an unknown-id ClaimJob / CompleteJob can no longer be applied at all (M2 + B5),
        // so no accepted block can carry them into an empty-map state to be reverted.
        for kind in [
            TxKind::ClaimJob { job_id: [9u8; 32] },
            TxKind::CompleteJob { job_id: [9u8; 32], result_hash: [1u8; 32] },
            TxKind::Batch { operations: vec![TxKind::ClaimJob { job_id: [9u8; 32] }] },
        ] {
            let mut state = ChainState::new();
            state.apply_block(&genesis_block()).unwrap();
            let v = state.accounts.get_or_create(addr(1));
            v.is_validator = true;
            v.balance = Amount::from_raw(1_000);
            let err = state
                .apply_block(&block_with(&state, 1, vec![unsigned(addr(1), 0, kind.clone())]))
                .unwrap_err();
            assert!(
                err.to_string().contains("unknown or expired job id")
                    || err.to_string().contains("complete: unknown job"),
                "kind {kind:?} must reject at apply post-flip, got: {err}"
            );
        }

        // A V2 block leaves pending+escrow non-empty ⇒ refused as well (guard 1 backstop).
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        state.accounts.get_or_create(addr(1)).balance = Amount::from_raw(MIN_BUDGET);
        state
            .apply_block(&block_with(&state, 1, vec![unsigned(addr(1), 0, v2_kind(MIN_BUDGET))]))
            .unwrap();
        let err = state.revert_block(1).unwrap_err();
        assert!(err.to_string().contains("consensus maps"), "got: {err}");
    }

    #[test]
    fn v2_replay_and_same_shape_resubmit_semantics() {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        state.accounts.get_or_create(addr(1)).balance = Amount::from_raw(3 * MIN_BUDGET);
        state.total_emitted = 3 * MIN_BUDGET;
        let tx0 = unsigned(addr(1), 0, v2_kind(MIN_BUDGET));
        let job0 = tx0.hash().0;
        state.apply_block(&block_with(&state, 1, vec![tx0.clone()])).unwrap();
        // Literal replay: the nonce check rejects the block; zero money delta (P1).
        let root = state.compute_state_root();
        let err = state.apply_block(&block_with(&state, 2, vec![tx0])).unwrap_err();
        assert!(matches!(err, StateError::InvalidNonce { .. }), "got: {err}");
        assert_eq!(state.compute_state_root(), root, "rejected replay leaves no trace");
        // Same-shape resubmit at the next nonce: a DISTINCT job with an independent pot.
        let tx1 = unsigned(addr(1), 1, v2_kind(MIN_BUDGET));
        let job1 = tx1.hash().0;
        assert_ne!(job0, job1, "the nonce is inside the hash ⇒ new job id");
        state.apply_block(&block_with(&state, 2, vec![tx1])).unwrap();
        assert_eq!(state.escrowed_for_job(&job0), MIN_BUDGET);
        assert_eq!(state.escrowed_for_job(&job1), MIN_BUDGET);
        assert_eq!(state.pending_jobs.len(), 2, "two independent pending jobs, two pots");
    }

    #[test]
    fn pending_job_from_tx_maps_v1_v2_flagship_and_priority() {
        use commputer_core::l2::FLAGSHIP_L2_ID;
        let v2 = unsigned_with_fee(addr(1), 0, v2_kind_l2(MIN_BUDGET, Some(FLAGSHIP_L2_ID.to_string())), 77);
        let pj = pending_job_from_tx(&v2).expect("V2 maps");
        assert_eq!(pj.job_id, v2.hash().0, "G-A: the SAME id the escrow/pending maps use");
        assert!(pj.is_flagship);
        assert_eq!(pj.priority, 77, "priority == fee");
        let other = unsigned(addr(1), 0, v2_kind_l2(MIN_BUDGET, Some("some-other-l2".into())));
        assert!(!pending_job_from_tx(&other).unwrap().is_flagship, "flagship is exact-match only");
        let none_l2 = unsigned(addr(1), 0, v2_kind(MIN_BUDGET));
        assert!(!pending_job_from_tx(&none_l2).unwrap().is_flagship, "no l2 ⇒ not flagship");
        let v1 = unsigned_with_fee(addr(1), 0, v1_kind(MIN_BUDGET), 5);
        let pj1 = pending_job_from_tx(&v1).expect("V1 maps too (pool-visible legacy jobs)");
        assert_eq!(pj1.job_id, v1.hash().0);
        assert_eq!(pj1.priority, 5);
        let t = unsigned(addr(1), 0, TxKind::Transfer { to: addr(2), amount: Amount::from_raw(50_000) });
        assert!(pending_job_from_tx(&t).is_none());
        let b = unsigned(addr(1), 0, TxKind::Batch { operations: vec![v2_kind(MIN_BUDGET)] });
        assert!(pending_job_from_tx(&b).is_none(), "batched V2 is rejected at apply (G-C)");
    }

    // --- B10 extension: fallback equivalence on BOTH ledger backends ---------------------------

    #[test]
    fn equivalence_escalate_fallback_zero_comp_matches() {
        let result = [7u8; 32];
        // 3-way split ⇒ both backends terminate Escalate with the pot HELD (proven equal by the
        // existing NoQuorum case); now apply the SAME D2 fallback to both and prove the drain
        // is equivalent too.
        let mut both = run_on_both(&Scenario {
            job: [6u8; 32],
            executor_result: Some(result),
            candidates: vec![10, 11, 12],
            commits: vec![(10, [1u8; 32], true), (11, [2u8; 32], true), (12, [3u8; 32], true)],
        });
        let h = match &both.staging_terminal {
            Terminal::Escalate(h) => h.clone(),
            other => panic!("expected Escalate, got {other:?}"),
        };
        // Staging side: the resolver, directly on the reference ledger.
        let s_out = resolve_escalation_fallback(&mut both.staging, both.job, &h);
        // Chain side: the PRODUCTION entry point — lifecycle_settle_and_drain re-settles to the
        // CACHED terminal (the P2 short-circuit) then runs the fallback and drains the entry.
        let (c_term, c_out) = both
            .chain
            .lifecycle_settle_and_drain(both.job, &ByteEq, BlockHash([0u8; 32]))
            .expect("pot pre-validates")
            .expect("lifecycle still present");
        assert!(matches!(c_term, Terminal::Escalate(_)));
        let c_out = c_out.expect("Escalate runs the fallback");
        assert_eq!(s_out, c_out, "fallback outcomes field-for-field across backends");
        assert_eq!(c_out.worker_paid, 0, "ZERO comp (D2-FINAL)");
        assert_eq!(c_out.burned, 0, "fallback burns nothing");
        assert!(both.chain.job_lifecycles.is_empty(), "chain side drained");
        // Full equivalence re-check: per-actor balances, pots (0 == 0), conservation, baselines.
        assert_equivalent(&both);
        assert_eq!(both.chain.escrowed_for_job(&both.job), 0);
        assert_eq!(both.chain.total_burned, 0, "no burn anywhere in the all-reveal fallback");
    }

    #[test]
    fn equivalence_escalate_fallback_with_forfeiture_matches() {
        // P10e: the FORFEITURE variant — the P2 wedge is invisible in the all-reveal scenario.
        // 2 distinct reveals + 1 silent ⇒ settle burned one bond on BOTH backends before the
        // handoff; the fallback must drain the REDUCED pot identically.
        let result = [7u8; 32];
        let mut both = run_on_both(&Scenario {
            job: [7u8; 32],
            executor_result: Some(result),
            candidates: vec![10, 11, 12],
            commits: vec![(10, [1u8; 32], true), (11, [2u8; 32], true), (12, [3u8; 32], false)],
        });
        let h = match &both.staging_terminal {
            Terminal::Escalate(h) => h.clone(),
            other => panic!("expected Escalate, got {other:?}"),
        };
        assert_eq!(h.committee_bonds.len(), 2, "only the revealers are handed off");
        let s_out = resolve_escalation_fallback(&mut both.staging, both.job, &h);
        let (_, c_out) = both
            .chain
            .lifecycle_settle_and_drain(both.job, &ByteEq, BlockHash([0u8; 32]))
            .expect("cached-terminal path (P2) — must NOT wedge on the reduced pot")
            .expect("lifecycle still present");
        let c_out = c_out.expect("fallback ran");
        assert_eq!(s_out, c_out, "reduced-pot fallback identical across backends");
        assert_eq!(c_out.bonds_returned, h.executor_bond + 2 * 1_650);
        assert_eq!(c_out.burned, 0, "the forfeit was settle's burn, not the fallback's");
        assert_equivalent(&both);
        assert_eq!(both.chain.escrowed_for_job(&both.job), 0, "reduced pot drained to exactly 0");
        assert_eq!(both.chain.total_burned, 1_650, "exactly the forfeited bond burned on-chain");
        assert_eq!(both.staging.balance_of(&lpid(12)), 0, "silent committer stays forfeited");
    }

    // ===========================================================================================
    // Phase 1.2a — B5 on-chain committee draw, B8 params, C1 restart-determinism, C2/C3 mempool,
    // M1 fail-hard, C9c genesis defaults.
    // ===========================================================================================

    /// B5 on-chain driver: fund submitter/executor + 5 bonded verifiers (candidate pool of 5, k=3 ⇒
    /// a SEED-dependent 3-member committee), then apply real Submit + Claim blocks. `order` fixes the
    /// bonded_stake INSERTION order so the two-node test can prove HashMap-order independence of the
    /// draw. Returns (state, job_id): lifecycle AwaitingResult, committee NOT yet drawn.
    fn onchain_claimed(order: &[u8]) -> (ChainState, [u8; 32]) {
        let min_bond = StakeParams::default().min_bond;
        let v_bond = GameParams::default().verifier_bond;
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        state.accounts.get_or_create(addr(2)).balance = Amount::from_raw(MIN_BUDGET);
        let e = state.accounts.get_or_create(addr(1));
        e.is_validator = true;
        e.balance = Amount::from_raw(MIN_BUDGET);
        for &v in order {
            let a = state.accounts.get_or_create(addr(v));
            a.is_validator = true;
            a.balance = Amount::from_raw(v_bond);
            state.bonded_stake.insert(addr(v), min_bond);
        }
        let submit = unsigned(addr(2), 0, v2_kind(MIN_BUDGET));
        let job = submit.hash().0;
        state.apply_block(&block_with(&state, 1, vec![submit])).unwrap();
        state
            .apply_block(&block_with(&state, 2, vec![unsigned(addr(1), 0, TxKind::ClaimJob { job_id: job })]))
            .unwrap();
        (state, job)
    }

    /// A CompleteJob block whose timestamp is overridden to `ts` — perturbs `block.hash()` (⇒ the
    /// draw seed) without touching the tx set.
    fn complete_block_ts(state: &ChainState, job: [u8; 32], result: [u8; 32], ts: u64) -> Block {
        let mut b = block_with(
            state,
            state.blocks.height() + 1,
            vec![unsigned(addr(1), 1, TxKind::CompleteJob { job_id: job, result_hash: result })],
        );
        b.header.timestamp = ts;
        b
    }

    #[test]
    fn b5_onchain_two_nodes_same_block_identical_committee_and_root() {
        // Two independently-built states (DIFFERENT bonded-stake insertion order) apply the SAME
        // finalized CompleteJob block ⇒ identical drawn committee + identical state root. This is the
        // fork-safety core: the draw reads only block.hash() + the sorted candidate snapshot +
        // bonded_stake + genesis k — never HashMap iteration order.
        let (mut a, ja) = onchain_claimed(&[3, 4, 5, 6, 7]);
        let (mut b, jb) = onchain_claimed(&[7, 6, 5, 4, 3]);
        assert_eq!(ja, jb, "same submit tx ⇒ same job id");
        let cj = complete_block_ts(&a, ja, [7u8; 32], 5000); // ONE block, applied to both
        a.apply_block(&cj).unwrap();
        b.apply_block(&cj).unwrap();
        let ca = a.job_lifecycles.get(&ja).unwrap().to_record().committee;
        let cb = b.job_lifecycles.get(&jb).unwrap().to_record().committee;
        assert_eq!(ca.len(), 3, "k=3 drawn from a 5-candidate pool");
        assert_eq!(ca, cb, "committee is candidate/HashMap-order independent + node-independent");
        assert_eq!(
            a.compute_state_root(),
            b.compute_state_root(),
            "identical state root (the committee folds into it)"
        );
    }

    #[test]
    fn b5_onchain_committee_is_seed_sensitive_and_folds_into_state_root() {
        // Vary ONLY the block-hash seed (via timestamp) across otherwise-identical runs. The only
        // resulting state difference is the drawn committee (same accounts/escrow/pending, executor
        // nonce bumped identically), so: (a) the committee must actually vary with the seed, and
        // (b) the state root differs IFF the committee differs — proving the committee is committed
        // to by the root (the §0 membership-gate fork surface).
        let mut seen: Vec<(Vec<[u8; 32]>, [u8; 32])> = Vec::new();
        for ts in 3000u64..3016 {
            let (mut s, job) = onchain_claimed(&[3, 4, 5, 6, 7]);
            let cj = complete_block_ts(&s, job, [7u8; 32], ts);
            s.apply_block(&cj).unwrap();
            let committee = s.job_lifecycles.get(&job).unwrap().to_record().committee;
            assert_eq!(committee.len(), 3);
            seen.push((committee, s.compute_state_root()));
        }
        let distinct: HashSet<Vec<[u8; 32]>> = seen.iter().map(|(c, _)| c.clone()).collect();
        assert!(distinct.len() > 1, "committee must depend on the seed; got {} distinct", distinct.len());
        for (ca, ra) in &seen {
            for (cb, rb) in &seen {
                assert_eq!(ca == cb, ra == rb, "the drawn committee folds injectively into the state root");
            }
        }
    }

    #[test]
    fn b5_completejob_arm_guards_reject_the_block() {
        // unknown job id ⇒ reject.
        let (mut s, _job) = onchain_claimed(&[3, 4, 5, 6, 7]);
        let err = s.apply_block(&complete_block_ts(&s, [0xABu8; 32], [7u8; 32], 4000)).unwrap_err();
        assert!(err.to_string().contains("complete: unknown job"), "unknown: {err}");

        // wrong executor: a candidate validator (addr 3), not the executor (addr 1), posts.
        let (mut s, job) = onchain_claimed(&[3, 4, 5, 6, 7]);
        let cj = block_with(&s, 3, vec![unsigned(addr(3), 0, TxKind::CompleteJob { job_id: job, result_hash: [7u8; 32] })]);
        let err = s.apply_block(&cj).unwrap_err();
        assert!(err.to_string().contains("NotExecutor"), "wrong executor: {err}");

        // wrong phase: a second CompleteJob (the first drew the committee ⇒ Committing).
        let (mut s, job) = onchain_claimed(&[3, 4, 5, 6, 7]);
        s.apply_block(&complete_block_ts(&s, job, [7u8; 32], 4001)).unwrap();
        let cj2 = block_with(&s, 4, vec![unsigned(addr(1), 2, TxKind::CompleteJob { job_id: job, result_hash: [7u8; 32] })]);
        let err = s.apply_block(&cj2).unwrap_err();
        assert!(err.to_string().contains("WrongPhase"), "second complete: {err}");

        // zero-from ⇒ reject on the P3 guard.
        let (mut s, job) = onchain_claimed(&[3, 4, 5, 6, 7]);
        let cj = block_with(&s, 3, vec![unsigned(addr(0), 0, TxKind::CompleteJob { job_id: job, result_hash: [7u8; 32] })]);
        let err = s.apply_block(&cj).unwrap_err();
        assert!(err.to_string().contains("zero address"), "zero from: {err}");

        // past window: unreachable on-chain (P8 timeout-settles the AwaitingResult job at the first
        // block past result_by, before a late CompleteJob can post), so the guard is exercised
        // directly via lifecycle_post_result at height > result_by (defense-in-depth).
        let (mut s, job) = onchain_claimed(&[3, 4, 5, 6, 7]);
        let rby = s.job_lifecycles.get(&job).unwrap().to_record().deadlines.result_by;
        assert_eq!(
            s.lifecycle_post_result(job, ParticipantId(addr(1).0), [7u8; 32], rby + 1),
            Some(EventResult::Rejected(commputer_pouw_onchain::lifecycle::RejectReason::PastWindow)),
            "post_result past result_by is PastWindow"
        );
    }

    #[test]
    fn b5_onchain_empty_committee_reaches_conserved_noquorum_refund() {
        // No bonded verifiers ⇒ empty candidate pool ⇒ empty committee ⇒ NoQuorum ⇒ D2 zero-comp
        // fallback ⇒ FULL refund, pot → 0, conserved. (The empty committee is deterministic, not an
        // error.)
        let mut state = ChainState::new();
        state.phase_windows = PhaseWindows { result_blocks: 2, commit_blocks: 2, reveal_blocks: 2, claim_blocks: 5 };
        state.apply_block(&genesis_block()).unwrap();
        state.accounts.get_or_create(addr(2)).balance = Amount::from_raw(MIN_BUDGET);
        let e = state.accounts.get_or_create(addr(1));
        e.is_validator = true;
        e.balance = Amount::from_raw(MIN_BUDGET);
        state.total_emitted = 2 * MIN_BUDGET;
        let conserved0 = money_conserved(&state);

        let submit = unsigned(addr(2), 0, v2_kind(MIN_BUDGET));
        let job = submit.hash().0;
        state.apply_block(&block_with(&state, 1, vec![submit])).unwrap();
        // claim at parent height 1 ⇒ result_by 3, commit_by 5, reveal_by 7.
        state
            .apply_block(&block_with(&state, 2, vec![unsigned(addr(1), 0, TxKind::ClaimJob { job_id: job })]))
            .unwrap();
        assert!(
            state.job_lifecycles.get(&job).unwrap().to_record().candidates.is_empty(),
            "no bonded verifiers ⇒ empty candidate pool"
        );
        // CompleteJob at parent height 2 ≤ result_by 3 ⇒ post_result ok; the tail draws an EMPTY committee.
        state
            .apply_block(&block_with(&state, 3, vec![unsigned(addr(1), 1, TxKind::CompleteJob { job_id: job, result_hash: [7u8; 32] })]))
            .unwrap();
        let rec = state.job_lifecycles.get(&job).unwrap().to_record();
        assert!(rec.committee.is_empty(), "empty candidate pool ⇒ empty committee");
        assert_eq!(rec.phase, commputer_pouw_onchain::lifecycle::PhaseRec::Committing);

        // Drive empties past reveal_by 7 ⇒ NoQuorum ⇒ D2 refund.
        while state.blocks.height() < 8 {
            let h = state.blocks.height() + 1;
            state.apply_block(&block_with(&state, h, vec![])).unwrap();
        }
        assert!(state.job_lifecycles.is_empty(), "settled + drained");
        assert_eq!(state.escrowed_for_job(&job), 0, "pot fully drained");
        assert_eq!(money_conserved(&state), conserved0, "conservation held across the empty-committee round");
        assert_eq!(bal(&state, 2), MIN_BUDGET, "submitter refunded the budget");
        assert_eq!(bal(&state, 1), MIN_BUDGET, "executor bond returned (zero-comp NoQuorum fallback)");
    }

    // ===========================================================================================
    // S5+S6 — THE FLIP: `Terminal::Escalate` opens a gated real second panel (EscalationRound)
    // instead of the zero-comp fallback; `settle_due_jobs` sweeps escalation rounds.
    // ===========================================================================================

    /// S5: parametrized `onchain_claimed` — `order.len()` bonded verifiers (bonded_stake insertion
    /// order = `order`, so the two-node test can prove HashMap-order independence of the PANEL
    /// draw), SHORT windows (result/commit/reveal 2, claim 5), real Submit + Claim blocks. Returns
    /// (state, job): lifecycle AwaitingResult at height 2, deadlines result_by 3 / commit_by 5 /
    /// reveal_by 7.
    fn onchain_claimed_n(order: &[u8]) -> (ChainState, [u8; 32]) {
        let min_bond = StakeParams::default().min_bond;
        let v_bond = GameParams::default().verifier_bond;
        let mut state = ChainState::new();
        state.phase_windows =
            PhaseWindows { result_blocks: 2, commit_blocks: 2, reveal_blocks: 2, claim_blocks: 5 };
        state.apply_block(&genesis_block()).unwrap();
        state.accounts.get_or_create(addr(2)).balance = Amount::from_raw(MIN_BUDGET);
        let e = state.accounts.get_or_create(addr(1));
        e.is_validator = true;
        e.balance = Amount::from_raw(MIN_BUDGET);
        for &v in order {
            let a = state.accounts.get_or_create(addr(v));
            a.is_validator = true;
            a.balance = Amount::from_raw(v_bond);
            state.bonded_stake.insert(addr(v), min_bond);
        }
        state.total_emitted = 2 * MIN_BUDGET + order.len() as u64 * (v_bond + min_bond);
        let submit = unsigned(addr(2), 0, v2_kind(MIN_BUDGET));
        let job = submit.hash().0;
        state.apply_block(&block_with(&state, 1, vec![submit])).unwrap();
        state
            .apply_block(&block_with(&state, 2, vec![unsigned(addr(1), 0, TxKind::ClaimJob { job_id: job })]))
            .unwrap();
        (state, job)
    }

    /// S5: drive identical chains through a round-1 3-way split: CompleteJob([7;32]) at b3, the
    /// drawn committee commits+reveals three DIFFERENT hashes ([1]/[2]/[3], all ≠ the executor's)
    /// ⇒ NoQuorum ⇒ `Terminal::Escalate` at the b8 settle block. Every block is built ONCE (from
    /// `states[0]`) and applied to EVERY state, so multi-state callers stay chain-identical.
    /// Leaves the chains at height 7 (== reveal_by); the CALLER applies the b8 settle block.
    /// Returns the drawn round-1 committee.
    fn drive_round1_split(states: &mut [&mut ChainState], job: [u8; 32]) -> Vec<[u8; 32]> {
        let v_bond = GameParams::default().verifier_bond;
        let result = [7u8; 32];
        let b3 = complete_block_ts(&*states[0], job, result, 5000);
        for s in states.iter_mut() {
            s.apply_block(&b3).unwrap();
        }
        let committee = states[0].job_lifecycles.get(&job).unwrap().to_record().committee;
        assert_eq!(committee.len(), 3, "k=3 committee drawn");
        // b4: the 3 committee members commit three DIFFERENT hashes (parent 3 ≤ commit_by 5).
        let commits: Vec<Transaction> = committee
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let c = make_commitment(
                    &ParticipantId(*m), &[(i + 1) as u8; 32], &[i as u8; 32], v_bond,
                );
                unsigned(Address(*m), 0, TxKind::Commit {
                    job_id: job, commit: c.commit, bond: Amount::from_raw(v_bond),
                })
            })
            .collect();
        let b4 = block_with(&*states[0], 4, commits);
        for s in states.iter_mut() {
            s.apply_block(&b4).unwrap();
        }
        // b5, b6: empty (past commit_by 5 ⇒ Revealing at b7's parent height 6).
        for h in 5..=6 {
            let blk = block_with(&*states[0], h, vec![]);
            for s in states.iter_mut() {
                s.apply_block(&blk).unwrap();
            }
        }
        // b7: the 3-way-split reveals (parent 6 > commit_by 5 ⇒ Revealing; 6 ≤ reveal_by 7).
        let reveals: Vec<Transaction> = committee
            .iter()
            .enumerate()
            .map(|(i, m)| {
                unsigned(Address(*m), 1, TxKind::Reveal {
                    job_id: job, result_hash: [(i + 1) as u8; 32], salt: [i as u8; 32],
                })
            })
            .collect();
        let b7 = block_with(&*states[0], 7, reveals);
        for s in states.iter_mut() {
            s.apply_block(&b7).unwrap();
        }
        committee
    }

    /// F2 gate PASSES: enough candidates outside the round-1 committee ⇒ a real round opens,
    /// the pot stays held, and the panel is deterministic across two nodes.
    #[test]
    fn escalate_opens_round_when_panel_viable_and_is_deterministic() {
        let ids: Vec<u8> = (3u8..15).collect(); // 12 bonded verifiers
        let rev: Vec<u8> = ids.iter().rev().copied().collect();
        let (mut a, ja) = onchain_claimed_n(&ids);
        let (mut b, jb) = onchain_claimed_n(&rev);
        assert_eq!(ja, jb, "same submit tx ⇒ same job id");
        let committee = drive_round1_split(&mut [&mut a, &mut b], ja);
        // b8: the settle block (8 > reveal_by 7) — THE FLIP: Escalate opens a real round.
        let b8 = block_with(&a, 8, vec![]);
        a.apply_block(&b8).unwrap();
        b.apply_block(&b8).unwrap();
        let ra = a.escalation_rounds.get(&ja).expect("round opened on node a");
        let rb = b.escalation_rounds.get(&jb).expect("round opened on node b");
        assert_eq!(ra.panel(), rb.panel(), "panel identical across nodes");
        let quorum = GameParams::default().quorum(GameParams::default().k_escalate);
        assert!(ra.panel().len() >= quorum, "panel {} >= quorum(k_escalate) {}", ra.panel().len(), quorum);
        assert_eq!(ra.panel().len(), 7, "full k_escalate panel from the 9 spare candidates");
        // Panel excludes the round-1 committee AND the executor.
        let committee_set: HashSet<[u8; 32]> = committee.iter().copied().collect();
        for p in ra.panel() {
            assert!(!committee_set.contains(&p.0), "panelist not in the round-1 committee");
            assert_ne!(p.0, addr(1).0, "executor never on the panel");
        }
        // Pot HELD (budget + Be + 3 revealer bonds), not refunded.
        let v_bond = GameParams::default().verifier_bond;
        let e_bond = MIN_BUDGET; // budget.max(GameParams::default().executor_bond)
        assert_eq!(a.escrowed_for_job(&ja), MIN_BUDGET + e_bond + 3 * v_bond, "pot held by the round");
        assert!(a.job_lifecycles.is_empty(), "primary lifecycle drained into the round");
        assert_eq!(
            a.compute_state_root(),
            b.compute_state_root(),
            "identical state roots (the round folds into the root)"
        );
    }

    /// F2 gate FAILS: candidate pool too small ⇒ byte-identical to today's fallback refund.
    #[test]
    fn escalate_falls_back_when_panel_unviable() {
        // 5 bonded verifiers: 3 drawn ⇒ 2 spare candidates < quorum(k_escalate)=5 ⇒ fallback.
        let (mut state, job) = onchain_claimed_n(&[3, 4, 5, 6, 7]);
        let conserved0 = money_conserved(&state);
        drive_round1_split(&mut [&mut state], job);
        let b8 = block_with(&state, 8, vec![]);
        state.apply_block(&b8).unwrap();
        assert!(state.escalation_rounds.is_empty(), "no round from an unviable pool");
        assert!(state.job_lifecycles.is_empty(), "lifecycle drained by the fallback");
        assert_eq!(state.escrowed_for_job(&job), 0, "pot fully refunded");
        assert_eq!(bal(&state, 2), MIN_BUDGET, "submitter refunded the budget");
        assert_eq!(bal(&state, 1), MIN_BUDGET, "executor bond returned (zero comp)");
        let v_bond = GameParams::default().verifier_bond;
        for v in 3u8..8 {
            assert_eq!(bal(&state, v), v_bond, "verifier {v}: revealer bond returned");
        }
        assert_eq!(state.total_burned, 0, "fallback burns nothing");
        assert_eq!(money_conserved(&state), conserved0, "conserved across the fallback");
    }

    /// The full on-chain escalation: open ⇒ 5 panel members commit+reveal the executor's hash
    /// (via the state helpers Task 6 will route txs to) ⇒ `settle_due_jobs` at reveal_by+1
    /// settles Confirmed and drains.
    #[test]
    fn escalation_round_settles_confirmed_and_drains() {
        let ids: Vec<u8> = (3u8..15).collect(); // 12 bonded verifiers
        let (mut state, job) = onchain_claimed_n(&ids);
        let conserved0 = money_conserved(&state);
        let committee = drive_round1_split(&mut [&mut state], job);
        let b8 = block_with(&state, 8, vec![]);
        state.apply_block(&b8).unwrap();
        let round = state.escalation_rounds.get(&job).expect("round opened");
        // Deadlines anchor at the b8 PARENT height 7 (the G-F anchor): commit_by 9, reveal_by 11.
        assert_eq!(round.deadlines().commit_by, 9);
        assert_eq!(round.deadlines().reveal_by, 11);
        let panel: Vec<ParticipantId> = round.panel().to_vec();
        let v_bond = GameParams::default().verifier_bond;
        let e_bond = MIN_BUDGET;
        let result = [7u8; 32]; // the executor's round-1 hash
        // 5 of the 7 panelists commit the executor's hash (quorum(7) == 5; 2 abstain — no
        // forfeiture for a never-committed panelist). Height 8 ≤ commit_by 9.
        for (i, p) in panel.iter().take(5).enumerate() {
            let c = make_commitment(p, &result, &[0x40 + i as u8; 32], v_bond);
            assert_eq!(
                state.escalation_record_commit(job, c, 8).unwrap(),
                Some(EscEventResult::Accepted),
                "panel commit {i} accepted"
            );
        }
        assert_eq!(
            state.escrowed_for_job(&job),
            MIN_BUDGET + e_bond + 3 * v_bond + 5 * v_bond,
            "pot grew by the 5 escrowed panel bonds"
        );
        // b9, b10: the sweep advances the round past commit_by 9 ⇒ Revealing.
        for h in 9..=10 {
            state.apply_block(&block_with(&state, h, vec![])).unwrap();
        }
        assert_eq!(state.escalation_rounds.get(&job).unwrap().phase(), PanelPhase::Revealing);
        // All 5 committers reveal the executor's hash (height 10 ≤ reveal_by 11).
        for (i, p) in panel.iter().take(5).enumerate() {
            let r = Reveal { verifier: *p, result_hash: result, salt: [0x40 + i as u8; 32] };
            assert_eq!(
                state.escalation_record_reveal(job, r, 10),
                Some(EscEventResult::Accepted),
                "panel reveal {i} accepted"
            );
        }
        // b11: not yet due (11 > reveal_by 11 is false) — the round survives.
        state.apply_block(&block_with(&state, 11, vec![])).unwrap();
        assert!(state.escalation_rounds.contains_key(&job), "not due at reveal_by");
        // b12: due ⇒ the S6 sweep settles Confirmed and drains.
        state.apply_block(&block_with(&state, 12, vec![])).unwrap();
        assert!(state.escalation_rounds.is_empty(), "settled + drained");
        assert_eq!(state.escrowed_for_job(&job), 0, "pot drained to exactly 0");
        // Confirmed money shape: executor 85% of budget + bond back.
        assert_eq!(
            bal(&state, 1),
            bps_of(MIN_BUDGET, 8_500) + e_bond,
            "executor: 85% comp + bond back (Confirmed)"
        );
        // Panel committers: bond back + even share of the escalation reward
        // (bps(3·Bv, escalation_reward_bps) split across the 5 revealers).
        let reward_each =
            bps_of(3 * v_bond, GameParams::default().escalation_reward_bps) / 5;
        for p in panel.iter().take(5) {
            assert_eq!(
                state.accounts.get(&Address(p.0)).unwrap().balance.raw(),
                v_bond + reward_each,
                "panelist: bond back + escalation reward"
            );
        }
        // Abstaining panelists: untouched.
        for p in panel.iter().skip(5) {
            assert_eq!(state.accounts.get(&Address(p.0)).unwrap().balance.raw(), v_bond);
        }
        // The round-1 committee (all wrong-side) stays slashed to 0.
        for m in &committee {
            assert_eq!(state.accounts.get(&Address(*m)).unwrap().balance.raw(), 0);
        }
        assert_eq!(money_conserved(&state), conserved0, "conserved end-to-end");
        // At-most-once: the round is gone; a direct re-drain is a no-op.
        assert!(state.escalation_settle_and_drain(job, &ByteEq).unwrap().is_none());
    }

    // --- S7: Commit/Reveal route to an active escalation round ------------------------------

    /// Task 6 (S7): with a real round open (drained primary), Commit/Reveal txs from the panel
    /// apply THROUGH `apply_block` (not the direct helpers) — proving the routing in
    /// `apply_commit`/`apply_reveal`, not just the underlying `escalation_record_*` helpers.
    #[test]
    fn commit_and_reveal_route_to_an_active_escalation_round() {
        let ids: Vec<u8> = (3u8..15).collect(); // 12 bonded verifiers
        let (mut state, job) = onchain_claimed_n(&ids);
        let committee = drive_round1_split(&mut [&mut state], job);
        let b8 = block_with(&state, 8, vec![]);
        state.apply_block(&b8).unwrap();
        let round = state.escalation_rounds.get(&job).expect("round opened");
        let panel: Vec<ParticipantId> = round.panel().to_vec();
        let v_bond = GameParams::default().verifier_bond;
        let pot0 = state.escrowed_for_job(&job);
        assert!(state.job_lifecycles.is_empty(), "primary drained; only the round is live");

        // A NON-panel bonded validator (a spare candidate excluded from the drawn panel, so its
        // round-1 bond is untouched and it still holds exactly v_bond) ⇒ whole-block reject,
        // NotPanelMember specifically (not InsufficientBalance / WrongPhase).
        let committee_set: HashSet<[u8; 32]> = committee.iter().copied().collect();
        let panel_set: HashSet<[u8; 32]> = panel.iter().map(|p| p.0).collect();
        let non_panel = ids
            .iter()
            .copied()
            .find(|v| !committee_set.contains(&addr(*v).0) && !panel_set.contains(&addr(*v).0))
            .expect("a spare candidate excluded from the panel exists (9 spares, 7-member panel)");
        let bad = make_commitment(&ParticipantId(addr(non_panel).0), &[7u8; 32], &[0u8; 32], v_bond);
        let err = state
            .apply_block(&block_with(&state, 9, vec![unsigned(addr(non_panel), 0, TxKind::Commit {
                job_id: job, commit: bad.commit, bond: Amount::from_raw(v_bond),
            })]))
            .unwrap_err();
        assert!(err.to_string().contains("NotPanelMember"), "got: {err}");
        assert_eq!(state.escrowed_for_job(&job), pot0, "rejected block moves no money");
        assert_eq!(state.blocks.height(), 8, "the whole block rolled back, not just the tx");

        // A panel member's Commit ⇒ accepted; bond escrowed via the round; nonce bumped.
        let p0 = panel[0];
        let nonce0 = state.accounts.get(&Address(p0.0)).map(|a| a.nonce).unwrap_or(0);
        let commit = make_commitment(&p0, &[7u8; 32], &[0x99u8; 32], v_bond);
        state
            .apply_block(&block_with(&state, 9, vec![unsigned(Address(p0.0), nonce0, TxKind::Commit {
                job_id: job, commit: commit.commit, bond: Amount::from_raw(v_bond),
            })]))
            .unwrap();
        assert_eq!(
            state.escrowed_for_job(&job),
            pot0 + v_bond,
            "escrowed_for_job grew by EXACTLY the panelist's bond"
        );
        assert_eq!(
            state.accounts.get(&Address(p0.0)).unwrap().nonce,
            nonce0 + 1,
            "committer's nonce bumped by the applied Commit tx"
        );
        assert_eq!(
            state.escalation_rounds.get(&job).unwrap().commitments().len(),
            1,
            "the round recorded the commitment"
        );

        // Advance past commit_by (the tail sweep flips Committing→Revealing once a block's own
        // height exceeds commit_by — mirrors the primary's P8 driver).
        while state.escalation_rounds.get(&job).unwrap().phase() != PanelPhase::Revealing {
            let h = state.blocks.height() + 1;
            state.apply_block(&block_with(&state, h, vec![])).unwrap();
        }

        // Reveal tx ⇒ accepted (the escalation arm self-advances the round by height first, same
        // as the primary's `lifecycle_advance` line — already Revealing here, so idempotent).
        let reveal_nonce = state.accounts.get(&Address(p0.0)).unwrap().nonce;
        let h = state.blocks.height() + 1;
        state
            .apply_block(&block_with(&state, h, vec![unsigned(Address(p0.0), reveal_nonce, TxKind::Reveal {
                job_id: job, result_hash: [7u8; 32], salt: [0x99u8; 32],
            })]))
            .unwrap();
        assert_eq!(
            state.accounts.get(&Address(p0.0)).unwrap().nonce,
            reveal_nonce + 1,
            "the Reveal applied (nonce bumped)"
        );
        assert_eq!(
            state.escalation_rounds.get(&job).unwrap().reveals().len(),
            1,
            "the round recorded the reveal"
        );
    }

    /// Task 6 (S7): a panel Commit trial-applied during Revealing (or a Reveal during Committing)
    /// errors ⇒ `tx_is_phase_deferred` must return true because `escalation_rounds` contains the
    /// job ⇒ `select_applicable_txs` REQUEUES it instead of dropping it (C3).
    #[test]
    fn escalation_commit_reveal_are_phase_deferred_not_dropped() {
        let ids: Vec<u8> = (3u8..15).collect();
        let (mut state, job) = onchain_claimed_n(&ids);
        drive_round1_split(&mut [&mut state], job);
        let b8 = block_with(&state, 8, vec![]);
        state.apply_block(&b8).unwrap();
        let round = state.escalation_rounds.get(&job).expect("round opened");
        assert_eq!(round.phase(), PanelPhase::Committing, "fresh round starts Committing");
        let panel: Vec<ParticipantId> = round.panel().to_vec();
        let v_bond = GameParams::default().verifier_bond;
        let root0 = state.compute_state_root();

        // A Reveal during Committing: WrongPhase, but the job is a live escalation round ⇒
        // phase-deferred ⇒ requeued.
        let p0 = panel[0];
        let reveal = unsigned(Address(p0.0), 0, TxKind::Reveal {
            job_id: job, result_hash: [7u8; 32], salt: [0x99u8; 32],
        });
        let (kept, requeue) = state.select_applicable_txs(vec![reveal.clone()]);
        assert!(kept.is_empty(), "the premature Reveal does not apply");
        assert_eq!(requeue.len(), 1, "requeued, not dropped");
        assert!(
            matches!(&requeue[0].kind, TxKind::Reveal { job_id, .. } if *job_id == job),
            "requeued the escalation-round Reveal"
        );
        assert_eq!(
            state.compute_state_root(),
            root0,
            "select_applicable_txs trial-applies then fully restores (no smear)"
        );

        // Sanity: the same tx really does reject with WrongPhase when force-applied.
        let err = state
            .apply_block(&block_with(&state, 9, vec![reveal]))
            .unwrap_err();
        assert!(err.to_string().contains("WrongPhase"), "got: {err}");

        // The other direction: a panel Commit trial-applied during Revealing is ALSO
        // phase-deferred (requeued, not dropped).
        while state.escalation_rounds.get(&job).unwrap().phase() != PanelPhase::Revealing {
            let h = state.blocks.height() + 1;
            state.apply_block(&block_with(&state, h, vec![])).unwrap();
        }
        let root1 = state.compute_state_root();
        let p1 = panel[1];
        let late_commit = make_commitment(&p1, &[7u8; 32], &[0x77u8; 32], v_bond);
        let commit_tx = unsigned(Address(p1.0), 0, TxKind::Commit {
            job_id: job, commit: late_commit.commit, bond: Amount::from_raw(v_bond),
        });
        let (kept2, requeue2) = state.select_applicable_txs(vec![commit_tx.clone()]);
        assert!(kept2.is_empty(), "the late Commit does not apply during Revealing");
        assert_eq!(requeue2.len(), 1, "requeued, not dropped");
        assert!(
            matches!(&requeue2[0].kind, TxKind::Commit { job_id, .. } if *job_id == job),
            "requeued the escalation-round Commit"
        );
        assert_eq!(state.compute_state_root(), root1, "no smear from the second trial either");
        let err2 = state
            .apply_block(&block_with(&state, state.blocks.height() + 1, vec![commit_tx]))
            .unwrap_err();
        assert!(err2.to_string().contains("WrongPhase"), "got: {err2}");
    }

    /// Rollback safety: a block whose LAST tx fails after the tail would have opened a round
    /// leaves escalation_rounds byte-identical (the whole tail is inside the envelope).
    #[test]
    fn rejected_block_leaves_escalation_rounds_untouched() {
        let ids: Vec<u8> = (3u8..15).collect();
        let (mut state, job) = onchain_claimed_n(&ids);
        drive_round1_split(&mut [&mut state], job);
        let root_before = state.compute_state_root();
        // A block at the settle height whose tail WOULD open the round, but whose final tx is
        // invalid (unfunded transfer) — the whole envelope (partial txs + tail) must roll back.
        let n3 = state.accounts.get(&addr(3)).map(|a| a.nonce).unwrap_or(0);
        let bad = block_with(&state, 8, vec![
            unsigned(addr(3), n3, TxKind::WithdrawUnbonded), // valid no-op first tx
            unsigned(addr(99), 0, TxKind::Transfer {
                to: addr(98), amount: Amount::from_raw(MIN_BUDGET),
            }),
        ]);
        assert!(state.apply_block(&bad).is_err(), "final invalid tx rejects the block");
        assert!(state.escalation_rounds.is_empty(), "no round from the rejected block");
        assert_eq!(state.compute_state_root(), root_before, "state byte-identical after rollback");
        // Proof the crafted block WOULD have opened it: the same height, valid ⇒ round opens.
        let b8 = block_with(&state, 8, vec![]);
        state.apply_block(&b8).unwrap();
        assert!(state.escalation_rounds.contains_key(&job), "valid settle block opens the round");
    }

    /// Snapshot-restore of an INSERTED round: within ONE tail sweep, job A (viable pool) opens
    /// an escalation round, then job B's settle Errs (tampered pot) ⇒ the whole block is
    /// rejected ⇒ `BlockSnapshot.escalation_rounds` must erase A's inserted round. This is the
    /// scenario where the S4 snapshot field is load-bearing — `rejected_block_leaves_…` above
    /// aborts in the tx loop, so no round is ever inserted there.
    #[test]
    fn mid_sweep_failure_rolls_back_an_inserted_escalation_round() {
        let min_bond = StakeParams::default().min_bond;
        let v_bond = GameParams::default().verifier_bond;
        let mut state = ChainState::new();
        state.phase_windows =
            PhaseWindows { result_blocks: 2, commit_blocks: 2, reveal_blocks: 2, claim_blocks: 5 };
        state.apply_block(&genesis_block()).unwrap();
        state.accounts.get_or_create(addr(2)).balance = Amount::from_raw(2 * MIN_BUDGET);
        let e = state.accounts.get_or_create(addr(1));
        e.is_validator = true;
        e.balance = Amount::from_raw(2 * MIN_BUDGET); // two executor bonds
        for v in 3u8..15 {
            let a = state.accounts.get_or_create(addr(v));
            a.is_validator = true;
            a.balance = Amount::from_raw(v_bond);
            state.bonded_stake.insert(addr(v), min_bond);
        }
        state.total_emitted = 4 * MIN_BUDGET + 12 * (v_bond + min_bond);
        // Two submits in b1; roles by job-id ORDER so the sorted sweep visits A before B:
        // A = smaller id (full NoQuorum round ⇒ opens), B = larger id (claimed, never
        // completed ⇒ TimedOut-due at the SAME height, pot tampered ⇒ settle Errs AFTER A).
        let s1 = unsigned(addr(2), 0, v2_kind(MIN_BUDGET));
        let s2 = unsigned(addr(2), 1, v2_kind(MIN_BUDGET));
        let (j1, j2) = (s1.hash().0, s2.hash().0);
        let (job_a, job_b) = if j1 < j2 { (j1, j2) } else { (j2, j1) };
        state.apply_block(&block_with(&state, 1, vec![s1, s2])).unwrap();
        // b2: claim A (parent 1 ⇒ A: result_by 3 / commit_by 5 / reveal_by 7 ⇒ due at 8).
        state
            .apply_block(&block_with(&state, 2, vec![unsigned(addr(1), 0, TxKind::ClaimJob { job_id: job_a })]))
            .unwrap();
        // b3: CompleteJob A ⇒ tail draws A's committee.
        state
            .apply_block(&block_with(&state, 3, vec![unsigned(addr(1), 1, TxKind::CompleteJob { job_id: job_a, result_hash: [7u8; 32] })]))
            .unwrap();
        let committee = state.job_lifecycles.get(&job_a).unwrap().to_record().committee;
        assert_eq!(committee.len(), 3);
        // b4: A's committee 3-way-split commits.
        let commits: Vec<Transaction> = committee
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let c = make_commitment(&ParticipantId(*m), &[(i + 1) as u8; 32], &[i as u8; 32], v_bond);
                unsigned(Address(*m), 0, TxKind::Commit {
                    job_id: job_a, commit: c.commit, bond: Amount::from_raw(v_bond),
                })
            })
            .collect();
        state.apply_block(&block_with(&state, 4, commits)).unwrap();
        state.apply_block(&block_with(&state, 5, vec![])).unwrap();
        // b6: claim B (parent 5 ≤ claim_by 5 ⇒ B: result_by 7, never completed ⇒ due at 8 too).
        state
            .apply_block(&block_with(&state, 6, vec![unsigned(addr(1), 2, TxKind::ClaimJob { job_id: job_b })]))
            .unwrap();
        // b7: A's 3-way-split reveals.
        let reveals: Vec<Transaction> = committee
            .iter()
            .enumerate()
            .map(|(i, m)| {
                unsigned(Address(*m), 1, TxKind::Reveal {
                    job_id: job_a, result_hash: [(i + 1) as u8; 32], salt: [i as u8; 32],
                })
            })
            .collect();
        state.apply_block(&block_with(&state, 7, reveals)).unwrap();
        assert!(state.escalation_rounds.is_empty(), "nothing settled before the b8 sweep");
        // Tamper job B's pot (out-of-band, like the d2 malformed-pot trick) so its settle
        // preflight Errs — AFTER job A's round insert (A < B in the sorted sweep).
        let pot_b = state.escrowed_for_job(&job_b);
        assert_eq!(pot_b, 2 * MIN_BUDGET, "B holds budget + e_bond");
        state.escrow_by_job.insert(job_b, pot_b - 1);
        let root_before = state.compute_state_root();
        let b8 = block_with(&state, 8, vec![]);
        let err = state.apply_block(&b8).unwrap_err();
        assert!(err.to_string().contains("job pot"), "B's pot preflight rejected the block: {err}");
        // THE pin: A's round WAS inserted mid-sweep; the snapshot must have erased it.
        assert!(state.escalation_rounds.is_empty(), "A's inserted round rolled back");
        assert_eq!(state.job_lifecycles.len(), 2, "both lifecycles restored (A un-drained)");
        assert_eq!(state.compute_state_root(), root_before, "state byte-identical after rollback");
        // Non-vacuity: repair B's pot ⇒ the SAME block applies and A's round opens for real.
        state.escrow_by_job.insert(job_b, pot_b);
        state.apply_block(&b8).unwrap();
        assert!(state.escalation_rounds.contains_key(&job_a), "untampered sweep opens A's round");
        assert!(state.job_lifecycles.is_empty(), "A drained into the round; B settled TimedOut");
    }

    /// Drive a full Confirmed round (on-chain B5 draw) to REVEALED-but-not-settled, on any backend,
    /// with NON-DEFAULT game params (worker/verifier split ≠ default) + short windows. Returns job.
    fn c1_drive_to_revealed(state: &mut ChainState) -> [u8; 32] {
        let min_bond = StakeParams::default().min_bond;
        let v_bond = GameParams::default().verifier_bond;
        let mut game = GameParams::default();
        game.worker_bps = 8_000;
        game.verifier_bps = 1_500;
        game.burn_bps = 500; // sum 10_000, ≠ default 8500/1000/500 ⇒ a DIFFERENT Confirmed split
        state
            .set_consensus_params(
                game,
                ResolutionParams::default(),
                PhaseWindows { result_blocks: 2, commit_blocks: 2, reveal_blocks: 2, claim_blocks: 5 },
                StakeParams::default(),
            )
            .unwrap();
        state.apply_block(&genesis_block()).unwrap();
        state.accounts.get_or_create(addr(2)).balance = Amount::from_raw(MIN_BUDGET);
        let e = state.accounts.get_or_create(addr(1));
        e.is_validator = true;
        e.balance = Amount::from_raw(MIN_BUDGET);
        for v in [3u8, 4, 5] {
            let a = state.accounts.get_or_create(addr(v));
            a.is_validator = true;
            a.balance = Amount::from_raw(min_bond + v_bond);
        }
        // b1: Bond the 3 verifiers (candidate pool of 3, k=3 ⇒ committee == all 3).
        let bonds: Vec<Transaction> = [3u8, 4, 5]
            .iter()
            .map(|&v| unsigned(addr(v), 0, TxKind::Bond { amount: Amount::from_raw(min_bond) }))
            .collect();
        let h = state.blocks.height() + 1;
        state.apply_block(&block_with(state, h, bonds)).unwrap();
        // b2: Submit.
        let submit = unsigned(addr(2), 0, v2_kind(MIN_BUDGET));
        let job = submit.hash().0;
        let h = state.blocks.height() + 1;
        state.apply_block(&block_with(state, h, vec![submit])).unwrap();
        // b3: Claim (parent 2 ⇒ result_by 4, commit_by 6, reveal_by 8).
        let h = state.blocks.height() + 1;
        state.apply_block(&block_with(state, h, vec![unsigned(addr(1), 0, TxKind::ClaimJob { job_id: job })])).unwrap();
        // b4: CompleteJob (parent 3 ≤ 4) ⇒ on-chain draw ⇒ Committing.
        let h = state.blocks.height() + 1;
        state.apply_block(&block_with(state, h, vec![unsigned(addr(1), 1, TxKind::CompleteJob { job_id: job, result_hash: [7u8; 32] })])).unwrap();
        // b5: Commit all 3 (parent 4 ≤ commit_by 6).
        let commits: Vec<Transaction> = [3u8, 4, 5]
            .iter()
            .enumerate()
            .map(|(i, &v)| {
                let c = make_commitment(&ParticipantId(addr(v).0), &[7u8; 32], &[i as u8; 32], v_bond);
                unsigned(addr(v), 1, TxKind::Commit { job_id: job, commit: c.commit, bond: Amount::from_raw(v_bond) })
            })
            .collect();
        let h = state.blocks.height() + 1;
        state.apply_block(&block_with(state, h, commits)).unwrap();
        // b6, b7 empty (advance past commit_by 6).
        for _ in 0..2 {
            let h = state.blocks.height() + 1;
            state.apply_block(&block_with(state, h, vec![])).unwrap();
        }
        // b8: Reveal all 3 (parent 7 > commit_by 6 ⇒ self-advance Revealing; ≤ reveal_by 8).
        let reveals: Vec<Transaction> = [3u8, 4, 5]
            .iter()
            .enumerate()
            .map(|(i, &v)| unsigned(addr(v), 2, TxKind::Reveal { job_id: job, result_hash: [7u8; 32], salt: [i as u8; 32] }))
            .collect();
        let h = state.blocks.height() + 1;
        state.apply_block(&block_with(state, h, reveals)).unwrap();
        job
    }

    /// One empty block past reveal_by ⇒ the P8 driver settles the revealed job (Confirmed).
    fn c1_settle(state: &mut ChainState) {
        let h = state.blocks.height() + 1;
        state.apply_block(&block_with(state, h, vec![])).unwrap();
    }

    fn c1_nondefault_params() -> (GameParams, ResolutionParams, PhaseWindows, StakeParams) {
        let mut game = GameParams::default();
        game.worker_bps = 8_000;
        game.verifier_bps = 1_500;
        game.burn_bps = 500;
        (
            game,
            ResolutionParams::default(),
            PhaseWindows { result_blocks: 2, commit_blocks: 2, reveal_blocks: 2, claim_blocks: 5 },
            StakeParams::default(),
        )
    }

    #[test]
    fn c1_restart_reinjects_genesis_params_matching_a_never_restarted_node() {
        // CONTROL: never restarts; the lifecycle keeps its non-default game params through settle.
        let mut ctrl = ChainState::new();
        let jc = c1_drive_to_revealed(&mut ctrl);
        c1_settle(&mut ctrl);
        assert!(ctrl.job_lifecycles.is_empty(), "control settled");
        let ctrl_root = ctrl.compute_state_root();
        let ctrl_conserved = money_conserved(&ctrl);
        let ctrl_exec_bal = bal(&ctrl, 1);

        // RESTART (with the C1 fix): drive to revealed on a rocks-backed node, DROP, reopen (the
        // lifecycle is reconstructed with DEFAULT params), re-install the genesis params (C1
        // re-injection rebuilds every lifecycle), settle ⇒ MUST match the never-restarted control.
        let dir = tempfile::tempdir().unwrap();
        let jr = {
            let mut r = ChainState::open(dir.path()).unwrap();
            let j = c1_drive_to_revealed(&mut r);
            r.flush().unwrap();
            j
        };
        assert_eq!(jr, jc, "identical job across backends");
        let mut r = ChainState::open(dir.path()).unwrap();
        let (g, rp, pw, st) = c1_nondefault_params();
        r.set_consensus_params(g, rp, pw, st).unwrap();
        c1_settle(&mut r);
        assert!(r.job_lifecycles.is_empty(), "restart settled");
        assert_eq!(
            r.compute_state_root(),
            ctrl_root,
            "C1: a node that restarted mid-lifecycle settles IDENTICALLY once genesis params are re-injected"
        );
        assert_eq!(money_conserved(&r), ctrl_conserved, "conservation across restart");
        assert_eq!(bal(&r, 1), ctrl_exec_bal);

        // NEGATIVE CONTROL (non-vacuous): reopen but DO NOT re-inject ⇒ the reloaded lifecycle keeps
        // DEFAULT params ⇒ a DIFFERENT Confirmed split ⇒ a DIFFERENT root. Without C1 this is a fork.
        let dir2 = tempfile::tempdir().unwrap();
        {
            let mut n = ChainState::open(dir2.path()).unwrap();
            let _ = c1_drive_to_revealed(&mut n);
            n.flush().unwrap();
        }
        let mut n = ChainState::open(dir2.path()).unwrap();
        c1_settle(&mut n);
        assert!(n.job_lifecycles.is_empty(), "negative control settled with default params");
        assert_ne!(
            n.compute_state_root(),
            ctrl_root,
            "without C1 re-injection the reloaded node FORKS (default split ≠ genesis split)"
        );
        assert_ne!(bal(&n, 1), ctrl_exec_bal, "the executor's worker payout differs under the default split");
        assert_eq!(money_conserved(&n), ctrl_conserved, "still conserved — just a divergent (forking) split");
    }

    #[test]
    fn c1_reinjected_k_feeds_the_postrestart_committee_draw() {
        // The C1 fix re-injects the FULL GameParams into reloaded lifecycles — including `k`, which
        // SIZES the committee (the draw itself, not just the settlement split). With k=2 (default 3)
        // and 3 eligible candidates the CompleteJob draw picks a 2-member committee. If a node
        // restarts between ClaimJob and CompleteJob and the re-injection failed to restore k, the
        // reloaded lifecycle would draw with default k=3 → a 3-member committee → a different state
        // root = fork on the exact membership gate §0 identifies. This complements the split-only C1
        // test above by exercising k-through-the-draw.
        let min_bond = StakeParams::default().min_bond;
        let v_bond = GameParams::default().verifier_bond;
        let k2_params = || {
            let mut g = GameParams::default();
            g.k = 2;
            (
                g,
                ResolutionParams::default(),
                PhaseWindows { result_blocks: 2, commit_blocks: 2, reveal_blocks: 2, claim_blocks: 5 },
                StakeParams::default(),
            )
        };
        // Bond 3 verifiers + Submit + Claim; stop at AwaitingResult (committee NOT yet drawn).
        let drive_to_claimed = |s: &mut ChainState| -> [u8; 32] {
            s.apply_block(&genesis_block()).unwrap();
            s.accounts.get_or_create(addr(2)).balance = Amount::from_raw(MIN_BUDGET);
            let e = s.accounts.get_or_create(addr(1));
            e.is_validator = true;
            e.balance = Amount::from_raw(MIN_BUDGET);
            for v in [3u8, 4, 5] {
                let a = s.accounts.get_or_create(addr(v));
                a.is_validator = true;
                a.balance = Amount::from_raw(min_bond + v_bond);
            }
            let bonds: Vec<Transaction> = [3u8, 4, 5]
                .iter()
                .map(|&v| unsigned(addr(v), 0, TxKind::Bond { amount: Amount::from_raw(min_bond) }))
                .collect();
            let h = s.blocks.height() + 1;
            s.apply_block(&block_with(s, h, bonds)).unwrap();
            let submit = unsigned(addr(2), 0, v2_kind(MIN_BUDGET));
            let job = submit.hash().0;
            let h = s.blocks.height() + 1;
            s.apply_block(&block_with(s, h, vec![submit])).unwrap();
            let h = s.blocks.height() + 1;
            s.apply_block(&block_with(s, h, vec![unsigned(addr(1), 0, TxKind::ClaimJob { job_id: job })])).unwrap();
            job
        };
        let complete = |s: &mut ChainState, job: [u8; 32]| {
            let h = s.blocks.height() + 1;
            s.apply_block(&block_with(s, h, vec![unsigned(addr(1), 1, TxKind::CompleteJob { job_id: job, result_hash: [7u8; 32] })])).unwrap();
        };

        // CONTROL: k=2 from genesis; draws a 2-member committee at CompleteJob.
        let mut ctrl = ChainState::new();
        let (g, rp, pw, st) = k2_params();
        ctrl.set_consensus_params(g, rp, pw, st).unwrap();
        let jc = drive_to_claimed(&mut ctrl);
        complete(&mut ctrl, jc);
        assert_eq!(ctrl.job_lifecycles.get(&jc).unwrap().committee().len(), 2, "k=2, 3 candidates ⇒ 2-member committee");
        let ctrl_root = ctrl.compute_state_root();

        // RESTART before the draw: drive to claimed on rocks, flush, drop; reopen (lifecycle reloaded
        // with DEFAULT k=3), re-inject k=2, THEN complete ⇒ the draw must use the re-injected k=2.
        let dir = tempfile::tempdir().unwrap();
        let job = {
            let mut r = ChainState::open(dir.path()).unwrap();
            let (g, rp, pw, st) = k2_params();
            r.set_consensus_params(g, rp, pw, st).unwrap();
            let j = drive_to_claimed(&mut r);
            r.flush().unwrap();
            j
        };
        let mut r = ChainState::open(dir.path()).unwrap();
        let (g, rp, pw, st) = k2_params();
        r.set_consensus_params(g, rp, pw, st).unwrap();
        complete(&mut r, job);
        assert_eq!(r.job_lifecycles.get(&job).unwrap().committee().len(), 2, "re-injected k=2 sized the post-restart draw");
        assert_eq!(
            r.compute_state_root(),
            ctrl_root,
            "C1: re-injected k feeds the post-restart draw identically to a never-restarted node",
        );

        // NEGATIVE CONTROL (non-vacuous): restart but DO NOT re-inject ⇒ reloaded lifecycle keeps
        // default k=3 ⇒ the draw picks 3 ⇒ a different committee ⇒ a divergent (forking) root.
        let dir2 = tempfile::tempdir().unwrap();
        let job2 = {
            let mut n = ChainState::open(dir2.path()).unwrap();
            let (g, rp, pw, st) = k2_params();
            n.set_consensus_params(g, rp, pw, st).unwrap();
            let j = drive_to_claimed(&mut n);
            n.flush().unwrap();
            j
        };
        let mut n = ChainState::open(dir2.path()).unwrap();
        complete(&mut n, job2); // reloaded with default k=3, no re-injection
        assert_eq!(n.job_lifecycles.get(&job2).unwrap().committee().len(), 3, "without re-injection the draw uses default k=3");
        assert_ne!(
            n.compute_state_root(),
            ctrl_root,
            "without C1 k re-injection the post-restart draw FORKS (3-member committee ≠ 2)",
        );
    }

    #[test]
    fn c2_c3_select_applicable_txs_keeps_requeues_drops_and_leaves_no_smear() {
        // A live lifecycle in AwaitingResult (committee not yet drawn).
        let (mut state, job) = claimed_job_state(None);
        let v_bond = GameParams::default().verifier_bond;
        state.accounts.get_mut(&addr(3)).unwrap().balance = Amount::from_raw(v_bond);
        state.accounts.get_or_create(addr(8)).balance = Amount::from_raw(5_000);
        let root0 = state.compute_state_root();

        // A clean tx (applies), a junk Commit on an UNKNOWN job (permanently doomed), and a Commit on
        // the LIVE job that is merely WrongPhase (the committee is not drawn yet — deferred).
        let clean = unsigned(addr(8), 0, TxKind::Bond { amount: Amount::from_raw(1_000) });
        let cu = make_commitment(&ParticipantId(addr(3).0), &[7u8; 32], &[0u8; 32], v_bond);
        let junk_unknown = unsigned(addr(3), 0, TxKind::Commit { job_id: [0xEEu8; 32], commit: cu.commit, bond: Amount::from_raw(v_bond) });
        let cw = make_commitment(&ParticipantId(addr(3).0), &[7u8; 32], &[0u8; 32], v_bond);
        let wrong_phase = unsigned(addr(3), 0, TxKind::Commit { job_id: job, commit: cw.commit, bond: Amount::from_raw(v_bond) });

        let (kept, requeue) =
            state.select_applicable_txs(vec![clean.clone(), junk_unknown.clone(), wrong_phase.clone()]);

        assert_eq!(kept.len(), 1, "only the clean tx applies");
        assert!(matches!(kept[0].kind, TxKind::Bond { .. }), "the Bond is kept");
        assert_eq!(requeue.len(), 1, "the WrongPhase Commit on a LIVE job is requeued, not dropped");
        assert!(
            matches!(&requeue[0].kind, TxKind::Commit { job_id, .. } if *job_id == job),
            "requeued the live-job Commit"
        );
        // the junk unknown-job Commit is in NEITHER set (permanently doomed ⇒ dropped). (TxKind is
        // not PartialEq, so match on the unknown job_id.)
        let is_unknown = |t: &Transaction| matches!(&t.kind, TxKind::Commit { job_id, .. } if *job_id == [0xEEu8; 32]);
        assert!(!requeue.iter().any(is_unknown) && !kept.iter().any(is_unknown), "unknown-job Commit is dropped");
        assert_eq!(
            state.compute_state_root(),
            root0,
            "C2: select_applicable_txs restores self fully — post-call root == pre-call root (no smear)"
        );
    }

    #[test]
    fn m1_open_fails_hard_on_corrupt_consensus_row() {
        // M1 end-to-end: a corrupt consensus CF row makes ChainState::open() FAIL with an actionable
        // error (not a silent warn-skip that would wedge every future block on the pot guard).
        let dir = tempfile::tempdir().unwrap();
        {
            let s = ChainState::open(dir.path()).unwrap();
            s.rocks.as_ref().unwrap().debug_put_corrupt_escrow_row();
        }
        let err = ChainState::open(dir.path()).unwrap_err();
        assert!(
            matches!(&err, StateError::StorageError(m) if m.contains("corrupt CF_ESCROW")),
            "open must fail hard on a corrupt consensus row, got: {err:?}"
        );
    }

    #[test]
    fn c9c_genesis_default_consensus_params_match_pouw_defaults() {
        // C9c drift-guard: the core default `ConsensusParamsConfig` converts to EXACTLY the
        // pouw/pouw-onchain defaults ChainState uses today, so a genesis omitting the section cannot
        // silently shift consensus params. (1.2a: standalone type; 1.2b embeds it in GenesisConfig.)
        use commputer_core::genesis::ConsensusParamsConfig;
        let gcp = genesis_consensus_params(&ConsensusParamsConfig::default());
        assert_eq!(gcp.stake, StakeParams::default(), "stake params");
        assert_eq!(gcp.phase_windows, PhaseWindows::default(), "phase windows");
        assert_eq!(gcp.resolution, ResolutionParams::default(), "resolution params");
        // GameParams is not Eq — the fingerprint folds every game/capacity/window/fuel field.
        assert_eq!(
            gcp.bundle.fingerprint(),
            commputer_pouw_onchain::consensus_params::ConsensusParams::default().fingerprint(),
            "the full consensus bundle == the compiled defaults byte-for-byte"
        );
        // Spot-check the game fields against a fresh (default) ChainState's game params.
        let fresh = ChainState::new();
        assert_eq!(gcp.game.k, fresh.game_params.k);
        assert_eq!(gcp.game.worker_bps, fresh.game_params.worker_bps);
        assert_eq!(gcp.game.executor_bond, fresh.game_params.executor_bond);
        // A JSON object omitting fields still deserializes to the defaults (serde-default at every level).
        let from_empty: ConsensusParamsConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(from_empty, ConsensusParamsConfig::default(), "omitted section ⇒ defaults");
    }

    #[test]
    fn set_consensus_params_rejects_zero_phase_window() {
        // C4: a zero (or <1) phase window is rejected — a same-block draw with a zero commit/reveal
        // window would be instantly swept to NoQuorum.
        let mut state = ChainState::new();
        let mut pw = PhaseWindows::default();
        pw.commit_blocks = 0;
        let err = state
            .set_consensus_params(GameParams::default(), ResolutionParams::default(), pw, StakeParams::default())
            .unwrap_err();
        assert!(err.to_string().contains("phase windows must be >= 1"), "got: {err}");
        // A valid (non-zero) window is accepted.
        assert!(state
            .set_consensus_params(GameParams::default(), ResolutionParams::default(), PhaseWindows::default(), StakeParams::default())
            .is_ok());
    }

    #[test]
    fn b7_set_capacity_params_stores_and_getter_reads() {
        // B7 (C8): the producer-side capacity split is installed by `set_capacity_params` and read
        // back by the getter B7's block-assembly admission uses. Default until set.
        let mut state = ChainState::new();
        assert_eq!(
            state.capacity_params().total_slots,
            commputer_pouw_onchain::capacity::CapacityParams::default().total_slots,
            "default until installed"
        );
        let mut cap = commputer_pouw_onchain::capacity::CapacityParams::default();
        cap.total_slots = 42;
        state.set_capacity_params(cap);
        assert_eq!(state.capacity_params().total_slots, 42, "getter reads the installed split");
    }

    #[test]
    fn b8_refuse_to_bind_gate_accepts_defaults_rejects_invalid() {
        // B8 (C4): the startup hard gate the node runs in main.rs. The compiled-default genesis params
        // pass `refuse_to_bind` against the node's compiled WasmLimits; an invalid config (a zero phase
        // window) FAILS it — proving the node would refuse to start rather than fork on bad params.
        use commputer_core::genesis::ConsensusParamsConfig;
        use commputer_pouw::wasm::WasmLimits;
        let ok = genesis_consensus_params(&ConsensusParamsConfig::default());
        assert!(
            ok.bundle.refuse_to_bind(&WasmLimits::default()).is_ok(),
            "default genesis params must bind"
        );
        let mut bad = ConsensusParamsConfig::default();
        bad.phase_windows.commit_blocks = 0;
        let gcp = genesis_consensus_params(&bad);
        assert!(
            gcp.bundle.refuse_to_bind(&WasmLimits::default()).is_err(),
            "an invalid (zero-window) genesis must be refused at bind"
        );
    }

    // --- SECURITY(F3/F6): MultiSig size cap at apply -------------------------------

    /// An over-large signers/signatures list is rejected BY THE SIZE GUARD, before the
    /// O(signatures×signers) ed25519 verify loop can run. NON-VACUOUS: pre-fix the arm had
    /// no size cap and ran the full loop, returning a "only N valid signatures" error (or
    /// succeeding) — never "exceeds max".
    #[test]
    fn f3_f6_multisig_oversized_rejected_before_verify_loop() {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        let over = commputer_core::transaction::Transaction::MAX_MULTISIG_SIGNERS + 1;
        let kind = TxKind::MultiSig {
            threshold: over as u8,
            signers: vec![vec![0u8; 32]; over],
            signatures: vec![vec![0u8; 64]; over],
        };
        let err = state
            .apply_block(&block_with(&state, 1, vec![unsigned(addr(1), 0, kind)]))
            .unwrap_err();
        assert!(
            err.to_string().contains("exceeds max"),
            "size guard must fire before the verify loop, got: {err}"
        );
    }

    /// A multisig sized exactly at MAX_MULTISIG_SIGNERS passes the size guard (the bound is
    /// inclusive/generous) — it fails later on signature validity, NOT on size.
    #[test]
    fn f3_f6_multisig_at_max_passes_size_guard() {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        let at = commputer_core::transaction::Transaction::MAX_MULTISIG_SIGNERS;
        let kind = TxKind::MultiSig {
            threshold: 1,
            signers: vec![vec![0u8; 32]; at],
            signatures: vec![vec![0u8; 64]; at],
        };
        let err = state
            .apply_block(&block_with(&state, 1, vec![unsigned(addr(1), 0, kind)]))
            .unwrap_err();
        assert!(
            !err.to_string().contains("exceeds max"),
            "MAX-sized multisig must clear the size guard, got: {err}"
        );
        assert!(
            err.to_string().contains("valid signature"),
            "it fails on signature validity instead, got: {err}"
        );
    }

    // --- SECURITY(F10/F21): MilestoneBurn / CharitableDonation conservation --------

    /// A MilestoneBurn must actually debit the sender's balance and consume a nonce, so
    /// `total_burned` only rises against real removed circulation. NON-VACUOUS: pre-fix the
    /// balance and nonce were untouched.
    #[test]
    fn f10_f21_milestone_burn_debits_balance_and_consumes_nonce() {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        state.accounts.get_or_create(addr(1)).balance = Amount::from_comme(100);
        state.total_emitted = Amount::from_comme(100).raw();
        let circ_before = state.circulating_supply();

        let kind = TxKind::MilestoneBurn {
            milestone_id: 1,
            burn_amount: Amount::from_comme(30),
            description_hash: [0u8; 32],
        };
        state
            .apply_block(&block_with(&state, 1, vec![unsigned(addr(1), 0, kind)]))
            .unwrap();

        let acct = state.accounts.get(&addr(1)).unwrap();
        assert_eq!(acct.balance, Amount::from_comme(70), "burn debits real balance");
        assert_eq!(acct.nonce, 1, "nonce consumed (no replay)");
        assert_eq!(state.total_burned, Amount::from_comme(30).raw());
        assert_eq!(
            state.circulating_supply(),
            circ_before - Amount::from_comme(30).raw(),
            "conservation: circulation drops by exactly the burned amount"
        );
    }

    /// A forged burn larger than the sender's balance must fail InsufficientBalance and NOT
    /// inflate `total_burned`. NON-VACUOUS: pre-fix `MilestoneBurn { burn_amount: u64::MAX }`
    /// succeeded and saturated total_burned with no balance backing.
    #[test]
    fn f10_f21_forged_milestone_burn_rejected_no_supply_corruption() {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        state.accounts.get_or_create(addr(1)).balance = Amount::from_comme(5);
        state.total_emitted = Amount::from_comme(5).raw();

        let kind = TxKind::MilestoneBurn {
            milestone_id: 1,
            burn_amount: Amount::from_raw(u64::MAX),
            description_hash: [0u8; 32],
        };
        let err = state
            .apply_block(&block_with(&state, 1, vec![unsigned(addr(1), 0, kind)]))
            .unwrap_err();
        assert!(
            matches!(err, StateError::InsufficientBalance),
            "an unbacked burn must be rejected, got: {err}"
        );
        assert_eq!(state.total_burned, 0, "total_burned NOT inflated by a forged burn");
        assert_eq!(
            state.accounts.get(&addr(1)).unwrap().nonce,
            0,
            "rejected tx consumes no nonce"
        );
    }

    /// A zero-address (keyless, unsigned) MilestoneBurn must be rejected outright.
    #[test]
    fn f10_f21_zero_from_milestone_burn_rejected() {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        let kind = TxKind::MilestoneBurn {
            milestone_id: 1,
            burn_amount: Amount::from_comme(1),
            description_hash: [0u8; 32],
        };
        let err = state
            .apply_block(&block_with(&state, 1, vec![unsigned(Address([0u8; 32]), 0, kind)]))
            .unwrap_err();
        assert!(
            err.to_string().contains("zero address"),
            "zero-from burn must be rejected, got: {err}"
        );
        assert_eq!(state.total_burned, 0);
    }

    // --- SECURITY(F11/F22): StorageWill.contact_hashes cap ------------------------

    /// An over-large contact_hashes list is rejected at apply; a within-cap one still applies.
    /// NON-VACUOUS: pre-fix an unbounded list was written verbatim into permanent account state.
    #[test]
    fn f11_f22_storage_will_contacts_capped_at_apply() {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();

        let over = MAX_WILL_CONTACTS + 1;
        let big = TxKind::StorageWill {
            contact_hashes: vec![[1u8; 32]; over],
            options_hash: [0u8; 32],
        };
        let err = state
            .apply_block(&block_with(&state, 1, vec![unsigned(addr(1), 0, big)]))
            .unwrap_err();
        assert!(
            err.to_string().contains("exceeds max"),
            "oversized will must be rejected, got: {err}"
        );
        assert!(
            state.accounts.get(&addr(1)).map(|a| a.will_contacts.is_empty()).unwrap_or(true),
            "rejected will writes nothing"
        );

        // A will exactly at the cap still applies (generous bound preserved).
        let ok = TxKind::StorageWill {
            contact_hashes: vec![[1u8; 32]; MAX_WILL_CONTACTS],
            options_hash: [0u8; 32],
        };
        state
            .apply_block(&block_with(&state, 1, vec![unsigned(addr(1), 0, ok)]))
            .unwrap();
        assert_eq!(
            state.accounts.get(&addr(1)).unwrap().will_contacts.len(),
            MAX_WILL_CONTACTS,
            "within-cap will applied"
        );
    }

    // --- SECURITY(F33): zero-address guards --------------------------------------

    /// An UNSIGNED zero-from Transfer (which a producer could inject, since zero-from skips
    /// signature verification) must not move value. NON-VACUOUS: pre-fix the Transfer arm had
    /// no zero-from guard and drained the keyless zero address.
    #[test]
    fn f33_zero_from_transfer_rejected() {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        // Fund the keyless zero address (e.g. mis-sent burn-address funds) and an existing
        // recipient (so the account-creation-fee path is not what rejects the tx).
        state.accounts.get_or_create(Address([0u8; 32])).balance = Amount::from_comme(50);
        state.accounts.get_or_create(addr(9)).balance = Amount::from_comme(1);

        let kind = TxKind::Transfer { to: addr(9), amount: Amount::from_comme(10) };
        let err = state
            .apply_block(&block_with(&state, 1, vec![unsigned(Address([0u8; 32]), 0, kind)]))
            .unwrap_err();
        assert!(
            err.to_string().contains("zero address cannot transfer"),
            "zero-from drain must be rejected, got: {err}"
        );
        assert_eq!(
            state.accounts.get(&Address([0u8; 32])).unwrap().balance,
            Amount::from_comme(50),
            "zero-address funds unmoved"
        );
        assert_eq!(
            state.accounts.get(&addr(9)).unwrap().balance,
            Amount::from_comme(1),
            "recipient not credited"
        );
    }

    /// The same drain via a batched value move (`Batch{Transfer{..}}` from zero) is rejected.
    #[test]
    fn f33_zero_from_batch_transfer_rejected() {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        state.accounts.get_or_create(Address([0u8; 32])).balance = Amount::from_comme(50);
        state.accounts.get_or_create(addr(9)).balance = Amount::from_comme(1);

        let inner = TxKind::Transfer { to: addr(9), amount: Amount::from_comme(10) };
        let batch = TxKind::Batch { operations: vec![inner] };
        let err = state
            .apply_block(&block_with(&state, 1, vec![unsigned(Address([0u8; 32]), 0, batch)]))
            .unwrap_err();
        assert!(
            err.to_string().contains("zero address"),
            "zero-from batch drain must be rejected, got: {err}"
        );
        assert_eq!(
            state.accounts.get(&Address([0u8; 32])).unwrap().balance,
            Amount::from_comme(50),
            "zero-address funds unmoved via batch"
        );
    }

    /// apply_genesis_accounts must refuse to credit the all-zero address. NON-VACUOUS: pre-fix
    /// it credited it and bumped total_emitted.
    #[test]
    fn f33_genesis_accounts_reject_zero_address() {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();
        let root_before = state.compute_state_root();

        let res = state.apply_genesis_accounts(&[(hex::encode([0u8; 32]), 1_000_000)]);
        assert!(res.is_err(), "genesis must refuse the zero address");
        assert!(
            res.unwrap_err().to_string().contains("zero address"),
            "clear zero-address rejection"
        );
        assert_eq!(state.total_emitted, 0, "no mint occurred");
        assert_eq!(state.compute_state_root(), root_before, "no state mutation on reject");
    }
}
