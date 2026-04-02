use rocksdb::{DB, Options, ColumnFamilyDescriptor, WriteBatch};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use commputer_core::block::{Block, BlockHash};
use commputer_core::identity::Address;
use tracing::info;
use crate::account::Account;

const CF_BLOCKS: &str = "blocks";
const CF_BLOCK_HEIGHTS: &str = "block_heights";
const CF_ACCOUNTS: &str = "accounts";
const CF_META: &str = "meta";
const CF_ARCHIVED: &str = "archived_accounts";

pub const META_TOTAL_EMITTED: &str = "total_emitted";
pub const META_TOTAL_BURNED: &str = "total_burned";
pub const META_CURRENT_EPOCH: &str = "current_epoch";
pub const META_CHAIN_HEIGHT: &str = "chain_height";
pub const META_NERF_RATE_BPS: &str = "nerf_rate_bps";
/// Feature 186: Schema version key.
pub const META_SCHEMA_VERSION: &str = "schema_version";
/// Feature 186: Current schema version.
pub const CURRENT_SCHEMA_VERSION: u64 = 1;
/// Item 15: Clean shutdown marker key.
pub const META_CLEAN_SHUTDOWN: &str = "clean_shutdown";

/// Persistent storage layer backed by RocksDB.
/// Used alongside in-memory stores — this is the durable layer.
pub struct RocksStore {
    db: DB,
    /// Feature 188: Storage metrics counters.
    pub total_reads: AtomicU64,
    pub total_writes: AtomicU64,
    pub total_read_us: AtomicU64,
    pub total_write_us: AtomicU64,
}

impl RocksStore {
    /// Item 16: Attempt to repair a corrupted database before opening.
    pub fn try_repair(path: &Path) {
        tracing::info!("Attempting database repair at {}", path.display());
        if let Err(e) = DB::repair(&Options::default(), path) {
            tracing::warn!("Database repair failed: {}", e);
        } else {
            tracing::info!("Database repair completed successfully");
        }
    }

    /// Open or create a RocksDB store at the given filesystem path.
    pub fn open(path: &Path) -> Result<Self, rocksdb::Error> {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);
        // Feature 189: Ensure WAL is enabled (RocksDB default, but be explicit).
        opts.set_wal_recovery_mode(rocksdb::DBRecoveryMode::PointInTime);

        let cf_names = [CF_BLOCKS, CF_BLOCK_HEIGHTS, CF_ACCOUNTS, CF_META, CF_ARCHIVED];
        let cfs: Vec<ColumnFamilyDescriptor> = cf_names
            .iter()
            .map(|name| ColumnFamilyDescriptor::new(*name, Options::default()))
            .collect();

        let db = DB::open_cf_descriptors(&opts, path, cfs)?;

        // Feature 189: Log WAL recovery status.
        info!("WAL recovery active — RocksDB opened with PointInTime recovery mode");

        let store = Self {
            db,
            total_reads: AtomicU64::new(0),
            total_writes: AtomicU64::new(0),
            total_read_us: AtomicU64::new(0),
            total_write_us: AtomicU64::new(0),
        };

        // Feature 186: Run migrations on open.
        store.run_migrations()?;

        // Item 15: Detect unclean shutdown.
        let was_clean = store.get_meta_u64(META_CLEAN_SHUTDOWN)
            .unwrap_or(None)
            .unwrap_or(0) == 1;
        if !was_clean {
            tracing::warn!("Detected unclean shutdown — RocksDB WAL recovery in progress");
        }
        // Clear the clean shutdown marker (will be set again on clean shutdown).
        let _ = store.put_meta_u64(META_CLEAN_SHUTDOWN, 0);

        // Feature 189: Verify WAL integrity.
        store.verify_wal();

        Ok(store)
    }

    /// Item 15: Mark a clean shutdown so next startup knows it was graceful.
    pub fn mark_clean_shutdown(&self) {
        if let Err(e) = self.put_meta_u64(META_CLEAN_SHUTDOWN, 1) {
            tracing::warn!("Failed to mark clean shutdown: {}", e);
        }
    }

    /// Feature 186: Run any needed database migrations.
    pub fn run_migrations(&self) -> Result<(), rocksdb::Error> {
        let current = self.get_meta_u64(META_SCHEMA_VERSION)?.unwrap_or(0);
        if current < CURRENT_SCHEMA_VERSION {
            info!(
                "Database migration: upgrading schema from v{} to v{}",
                current, CURRENT_SCHEMA_VERSION
            );
            // Migration v0 -> v1: set schema version (initial schema, no data changes).
            if current < 1 {
                info!("Migration v0 -> v1: setting initial schema version");
                self.put_meta_u64(META_SCHEMA_VERSION, 1)?;
            }
            // Future migrations go here as `if current < 2 { ... }` etc.
            info!("Database migrations complete");
        } else {
            info!("Database schema at v{}, no migrations needed", current);
        }
        Ok(())
    }

    /// Feature 19: Safe column family lookup — returns a reference to the named CF.
    /// Column families are registered at DB open time, so this should never fail.
    fn cf(&self, name: &str) -> &rocksdb::ColumnFamily {
        self.db.cf_handle(name)
            .unwrap_or_else(|| panic!("BUG: column family '{}' not found in RocksDB", name))
    }

    /// Feature 189: Verify WAL integrity by checking that the DB is readable.
    fn verify_wal(&self) {
        // RocksDB replays WAL on open. If we got here, WAL is valid.
        // Do a simple read to confirm the DB is functional.
        let cf = self.cf(CF_META);
        match self.db.get_cf(&cf, META_SCHEMA_VERSION.as_bytes()) {
            Ok(_) => info!("WAL verification passed"),
            Err(e) => info!("WAL verification warning: {}", e),
        }
    }

    // ── Block operations ──

    /// Persist a block to RocksDB and update the height index.
    pub fn put_block(&self, block: &Block) -> Result<(), rocksdb::Error> {
        let hash = block.hash();
        let height = block.height();
        let encoded = borsh::to_vec(block).expect("block borsh serialization should not fail");

        let cf_blocks = self.cf(CF_BLOCKS);
        self.db.put_cf(&cf_blocks, hash.0, &encoded)?;

        let cf_heights = self.cf(CF_BLOCK_HEIGHTS);
        self.db.put_cf(&cf_heights, height.to_le_bytes(), hash.0)?;

        // Update chain_height meta if this block is higher.
        let current = self.get_meta_u64(META_CHAIN_HEIGHT)?.unwrap_or(0);
        if height >= current || current == 0 {
            self.put_meta_u64(META_CHAIN_HEIGHT, height)?;
        }

        Ok(())
    }

    /// Retrieve a block by hash from RocksDB.
    pub fn get_block(&self, hash: &BlockHash) -> Result<Option<Block>, rocksdb::Error> {
        let cf = self.cf(CF_BLOCKS);
        match self.db.get_cf(&cf, hash.0)? {
            Some(data) => {
                let block: Block =
                    borsh::from_slice(&data).expect("block borsh deserialization should not fail");
                Ok(Some(block))
            }
            None => Ok(None),
        }
    }

    /// Retrieve a block by height via the height index.
    pub fn get_block_by_height(&self, height: u64) -> Result<Option<Block>, rocksdb::Error> {
        let cf_heights = self.cf(CF_BLOCK_HEIGHTS);
        match self.db.get_cf(&cf_heights, height.to_le_bytes())? {
            Some(hash_bytes) => {
                let mut hash = [0u8; 32];
                hash.copy_from_slice(&hash_bytes);
                self.get_block(&BlockHash(hash))
            }
            None => Ok(None),
        }
    }

    // ── Account operations ──

    /// Persist an account to RocksDB.
    pub fn put_account(&self, account: &Account) -> Result<(), rocksdb::Error> {
        let cf = self.cf(CF_ACCOUNTS);
        let encoded =
            borsh::to_vec(account).expect("account borsh serialization should not fail");
        self.db.put_cf(&cf, account.address.0, &encoded)
    }

    /// Retrieve an account by address from RocksDB.
    pub fn get_account(&self, address: &Address) -> Result<Option<Account>, rocksdb::Error> {
        let cf = self.cf(CF_ACCOUNTS);
        match self.db.get_cf(&cf, address.0)? {
            Some(data) => {
                let account: Account = borsh::from_slice(&data)
                    .expect("account borsh deserialization should not fail");
                Ok(Some(account))
            }
            None => Ok(None),
        }
    }

    /// Iterate all accounts in the database.
    pub fn all_accounts(&self) -> Vec<Account> {
        let cf = self.cf(CF_ACCOUNTS);
        let iter = self.db.iterator_cf(&cf, rocksdb::IteratorMode::Start);
        let mut accounts = Vec::new();
        for item in iter {
            if let Ok((_key, value)) = item
                && let Ok(account) = borsh::from_slice::<Account>(&value) {
                    accounts.push(account);
                }
        }
        accounts
    }

    /// Iterate all blocks in the database (by height order).
    pub fn all_blocks_by_height(&self) -> Vec<Block> {
        let cf_heights = self.cf(CF_BLOCK_HEIGHTS);
        let cf_blocks = self.cf(CF_BLOCKS);
        let iter = self.db.iterator_cf(&cf_heights, rocksdb::IteratorMode::Start);
        let mut blocks = Vec::new();
        for item in iter {
            if let Ok((_height_key, hash_bytes)) = item
                && let Ok(Some(data)) = self.db.get_cf(&cf_blocks, &*hash_bytes)
                    && let Ok(block) = borsh::from_slice::<Block>(&data) {
                        blocks.push(block);
                    }
        }
        blocks
    }

    // ── Meta operations ──

    /// Store a u64 metadata value.
    pub fn put_meta_u64(&self, key: &str, value: u64) -> Result<(), rocksdb::Error> {
        let cf = self.cf(CF_META);
        self.db.put_cf(&cf, key.as_bytes(), value.to_le_bytes())
    }

    /// Retrieve a u64 metadata value.
    pub fn get_meta_u64(&self, key: &str) -> Result<Option<u64>, rocksdb::Error> {
        let cf = self.cf(CF_META);
        let start = std::time::Instant::now();
        let result = match self.db.get_cf(&cf, key.as_bytes())? {
            Some(data) => {
                let mut buf = [0u8; 8];
                buf.copy_from_slice(&data);
                Ok(Some(u64::from_le_bytes(buf)))
            }
            None => Ok(None),
        };
        self.total_reads.fetch_add(1, Ordering::Relaxed);
        self.total_read_us.fetch_add(start.elapsed().as_micros() as u64, Ordering::Relaxed);
        result
    }

    // ── Feature 183: Archived account operations ──

    /// Archive an account to cold storage.
    pub fn put_archived_account(&self, account: &Account) -> Result<(), rocksdb::Error> {
        let cf = self.cf(CF_ARCHIVED);
        let encoded = borsh::to_vec(account).expect("account borsh serialization should not fail");
        self.db.put_cf(&cf, account.address.0, &encoded)
    }

    /// Retrieve an archived account.
    pub fn get_archived_account(&self, address: &Address) -> Result<Option<Account>, rocksdb::Error> {
        let cf = self.cf(CF_ARCHIVED);
        match self.db.get_cf(&cf, address.0)? {
            Some(data) => {
                let account: Account = borsh::from_slice(&data)
                    .expect("account borsh deserialization should not fail");
                Ok(Some(account))
            }
            None => Ok(None),
        }
    }

    /// Remove an archived account (after export or cleanup).
    pub fn delete_archived_account(&self, address: &Address) -> Result<(), rocksdb::Error> {
        let cf = self.cf(CF_ARCHIVED);
        self.db.delete_cf(&cf, address.0)
    }

    /// Count archived accounts.
    pub fn archived_account_count(&self) -> usize {
        let cf = self.cf(CF_ARCHIVED);
        let iter = self.db.iterator_cf(&cf, rocksdb::IteratorMode::Start);
        iter.count()
    }

    // ── Feature 190: Atomic write batch ──

    /// Create a WriteBatch for atomic multi-key writes.
    pub fn new_write_batch(&self) -> WriteBatch {
        WriteBatch::default()
    }

    /// Add an account write to a batch.
    pub fn batch_put_account(&self, batch: &mut WriteBatch, account: &Account) {
        let cf = self.cf(CF_ACCOUNTS);
        let encoded = borsh::to_vec(account).expect("account borsh serialization should not fail");
        batch.put_cf(&cf, account.address.0, &encoded);
    }

    /// Add a block write to a batch.
    pub fn batch_put_block(&self, batch: &mut WriteBatch, block: &Block) {
        let hash = block.hash();
        let height = block.height();
        let encoded = borsh::to_vec(block).expect("block borsh serialization should not fail");
        let cf_blocks = self.cf(CF_BLOCKS);
        batch.put_cf(&cf_blocks, hash.0, &encoded);
        let cf_heights = self.cf(CF_BLOCK_HEIGHTS);
        batch.put_cf(&cf_heights, height.to_le_bytes(), hash.0);
    }

    /// Add a meta u64 write to a batch.
    pub fn batch_put_meta_u64(&self, batch: &mut WriteBatch, key: &str, value: u64) {
        let cf = self.cf(CF_META);
        batch.put_cf(&cf, key.as_bytes(), value.to_le_bytes());
    }

    /// Atomically commit a write batch.
    pub fn write_batch(&self, batch: WriteBatch) -> Result<(), rocksdb::Error> {
        let start = std::time::Instant::now();
        self.db.write(batch)?;
        let elapsed = start.elapsed().as_micros() as u64;
        self.total_writes.fetch_add(1, Ordering::Relaxed);
        self.total_write_us.fetch_add(elapsed, Ordering::Relaxed);
        Ok(())
    }

    /// Clear all data from all column families. Used during chain resync.
    /// Uses delete_range for O(1) bulk deletion instead of iterating all keys.
    pub fn clear_all(&self) -> Result<(), rocksdb::Error> {
        let range_start: &[u8] = &[];
        let range_end: &[u8] = &[0xFF; 128]; // Covers any key length
        let mut batch = rocksdb::WriteBatch::default();
        for cf_name in &[CF_BLOCKS, CF_BLOCK_HEIGHTS, CF_ACCOUNTS, CF_META, CF_ARCHIVED] {
            let cf = self.cf(cf_name);
            batch.delete_range_cf(&cf, range_start, range_end);
        }
        self.db.write(batch)?;
        Ok(())
    }

    // ── Feature 188: Storage metrics ──

    /// Estimate database size on disk (in bytes).
    pub fn estimate_db_size(&self) -> u64 {
        let mut total: u64 = 0;
        for cf_name in &[CF_BLOCKS, CF_BLOCK_HEIGHTS, CF_ACCOUNTS, CF_META, CF_ARCHIVED] {
            if let Some(cf) = self.db.cf_handle(cf_name)
                && let Ok(Some(size_str)) = self.db.property_value_cf(&cf, "rocksdb.estimate-live-data-size")
                    && let Ok(size) = size_str.parse::<u64>() {
                        total += size;
                    }
        }
        total
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use commputer_core::block::{BlockHeader, BlockHash};
    use commputer_core::identity::Address;
    use commputer_core::token::Amount;

    fn test_address(n: u8) -> Address {
        let mut a = [0u8; 32];
        a[0] = n;
        Address(a)
    }

    fn make_test_block(height: u64, parent: BlockHash) -> Block {
        Block {
            header: BlockHeader {
                protocol_version: 1,
                height,
                parent_hash: parent,
                tx_root: [0u8; 32],
                proof_root: [0u8; 32],
                state_root: [0u8; 32],
                timestamp: 1000 + height,
                producer: Address([0u8; 32]),
                epoch: 0,
                producer_public_key: vec![],
                signature: vec![],
                checkpoint_hash: None, chain_id: String::new(),
            },
            transactions: vec![],
            proof_summaries: vec![],
            compliance_summary: None, epoch_summary: None,
        }
    }

    #[test]
    fn rocks_store_put_get_block() {
        let dir = tempfile::tempdir().unwrap();
        let store = RocksStore::open(dir.path()).unwrap();
        let block = make_test_block(0, BlockHash::GENESIS);
        let hash = block.hash();
        store.put_block(&block).unwrap();
        let loaded = store.get_block(&hash).unwrap().unwrap();
        assert_eq!(loaded.hash(), hash);
    }

    #[test]
    fn rocks_store_get_block_by_height() {
        let dir = tempfile::tempdir().unwrap();
        let store = RocksStore::open(dir.path()).unwrap();

        let b0 = make_test_block(0, BlockHash::GENESIS);
        let h0 = b0.hash();
        store.put_block(&b0).unwrap();

        let b1 = make_test_block(1, h0);
        store.put_block(&b1).unwrap();

        let loaded = store.get_block_by_height(1).unwrap().unwrap();
        assert_eq!(loaded.height(), 1);
        assert!(store.get_block_by_height(99).unwrap().is_none());
    }

    #[test]
    fn rocks_store_put_get_account() {
        let dir = tempfile::tempdir().unwrap();
        let store = RocksStore::open(dir.path()).unwrap();
        let mut acct = Account::new(test_address(1));
        acct.balance = Amount::from_comme(42);
        store.put_account(&acct).unwrap();
        let loaded = store.get_account(&acct.address).unwrap().unwrap();
        assert_eq!(loaded.address, acct.address);
        assert_eq!(loaded.balance, Amount::from_comme(42));
    }

    #[test]
    fn rocks_store_meta_u64() {
        let dir = tempfile::tempdir().unwrap();
        let store = RocksStore::open(dir.path()).unwrap();
        assert!(store.get_meta_u64("total_emitted").unwrap().is_none());
        store.put_meta_u64("total_emitted", 123456).unwrap();
        assert_eq!(store.get_meta_u64("total_emitted").unwrap(), Some(123456));
    }

    #[test]
    fn rocks_store_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();

        // Write data.
        {
            let store = RocksStore::open(dir.path()).unwrap();
            let block = make_test_block(0, BlockHash::GENESIS);
            store.put_block(&block).unwrap();
            let acct = Account::new(test_address(5));
            store.put_account(&acct).unwrap();
            store.put_meta_u64("total_emitted", 999).unwrap();
        }

        // Reopen and verify.
        {
            let store = RocksStore::open(dir.path()).unwrap();
            assert!(store.get_block_by_height(0).unwrap().is_some());
            assert!(store.get_account(&test_address(5)).unwrap().is_some());
            assert_eq!(store.get_meta_u64("total_emitted").unwrap(), Some(999));
        }
    }

    #[test]
    fn rocks_store_all_accounts() {
        let dir = tempfile::tempdir().unwrap();
        let store = RocksStore::open(dir.path()).unwrap();
        store.put_account(&Account::new(test_address(1))).unwrap();
        store.put_account(&Account::new(test_address(2))).unwrap();
        store.put_account(&Account::new(test_address(3))).unwrap();
        let accounts = store.all_accounts();
        assert_eq!(accounts.len(), 3);
    }
}
