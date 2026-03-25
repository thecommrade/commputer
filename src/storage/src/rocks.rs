use rocksdb::{DB, Options, ColumnFamilyDescriptor};
use std::path::Path;
use commputer_core::block::{Block, BlockHash};
use commputer_core::identity::Address;
use crate::account::Account;

const CF_BLOCKS: &str = "blocks";
const CF_BLOCK_HEIGHTS: &str = "block_heights";
const CF_ACCOUNTS: &str = "accounts";
const CF_META: &str = "meta";

pub const META_TOTAL_EMITTED: &str = "total_emitted";
pub const META_TOTAL_BURNED: &str = "total_burned";
pub const META_CURRENT_EPOCH: &str = "current_epoch";
pub const META_CHAIN_HEIGHT: &str = "chain_height";
pub const META_NERF_RATE_BPS: &str = "nerf_rate_bps";

/// Persistent storage layer backed by RocksDB.
/// Used alongside in-memory stores — this is the durable layer.
pub struct RocksStore {
    db: DB,
}

impl RocksStore {
    pub fn open(path: &Path) -> Result<Self, rocksdb::Error> {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);

        let cf_names = vec![CF_BLOCKS, CF_BLOCK_HEIGHTS, CF_ACCOUNTS, CF_META];
        let cfs: Vec<ColumnFamilyDescriptor> = cf_names
            .iter()
            .map(|name| ColumnFamilyDescriptor::new(*name, Options::default()))
            .collect();

        let db = DB::open_cf_descriptors(&opts, path, cfs)?;
        Ok(Self { db })
    }

    // ── Block operations ──

    pub fn put_block(&self, block: &Block) -> Result<(), rocksdb::Error> {
        let hash = block.hash();
        let height = block.height();
        let encoded = borsh::to_vec(block).expect("block borsh serialization should not fail");

        let cf_blocks = self.db.cf_handle(CF_BLOCKS).unwrap();
        self.db.put_cf(&cf_blocks, hash.0, &encoded)?;

        let cf_heights = self.db.cf_handle(CF_BLOCK_HEIGHTS).unwrap();
        self.db.put_cf(&cf_heights, height.to_le_bytes(), hash.0)?;

        // Update chain_height meta if this block is higher.
        let current = self.get_meta_u64(META_CHAIN_HEIGHT)?.unwrap_or(0);
        if height >= current || current == 0 {
            self.put_meta_u64(META_CHAIN_HEIGHT, height)?;
        }

        Ok(())
    }

    pub fn get_block(&self, hash: &BlockHash) -> Result<Option<Block>, rocksdb::Error> {
        let cf = self.db.cf_handle(CF_BLOCKS).unwrap();
        match self.db.get_cf(&cf, hash.0)? {
            Some(data) => {
                let block: Block =
                    borsh::from_slice(&data).expect("block borsh deserialization should not fail");
                Ok(Some(block))
            }
            None => Ok(None),
        }
    }

    pub fn get_block_by_height(&self, height: u64) -> Result<Option<Block>, rocksdb::Error> {
        let cf_heights = self.db.cf_handle(CF_BLOCK_HEIGHTS).unwrap();
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

    pub fn put_account(&self, account: &Account) -> Result<(), rocksdb::Error> {
        let cf = self.db.cf_handle(CF_ACCOUNTS).unwrap();
        let encoded =
            borsh::to_vec(account).expect("account borsh serialization should not fail");
        self.db.put_cf(&cf, account.address.0, &encoded)
    }

    pub fn get_account(&self, address: &Address) -> Result<Option<Account>, rocksdb::Error> {
        let cf = self.db.cf_handle(CF_ACCOUNTS).unwrap();
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
        let cf = self.db.cf_handle(CF_ACCOUNTS).unwrap();
        let iter = self.db.iterator_cf(&cf, rocksdb::IteratorMode::Start);
        let mut accounts = Vec::new();
        for item in iter {
            if let Ok((_key, value)) = item {
                if let Ok(account) = borsh::from_slice::<Account>(&value) {
                    accounts.push(account);
                }
            }
        }
        accounts
    }

    /// Iterate all blocks in the database (by height order).
    pub fn all_blocks_by_height(&self) -> Vec<Block> {
        let cf_heights = self.db.cf_handle(CF_BLOCK_HEIGHTS).unwrap();
        let cf_blocks = self.db.cf_handle(CF_BLOCKS).unwrap();
        let iter = self.db.iterator_cf(&cf_heights, rocksdb::IteratorMode::Start);
        let mut blocks = Vec::new();
        for item in iter {
            if let Ok((_height_key, hash_bytes)) = item {
                if let Ok(Some(data)) = self.db.get_cf(&cf_blocks, &*hash_bytes) {
                    if let Ok(block) = borsh::from_slice::<Block>(&data) {
                        blocks.push(block);
                    }
                }
            }
        }
        blocks
    }

    // ── Meta operations ──

    pub fn put_meta_u64(&self, key: &str, value: u64) -> Result<(), rocksdb::Error> {
        let cf = self.db.cf_handle(CF_META).unwrap();
        self.db.put_cf(&cf, key.as_bytes(), value.to_le_bytes())
    }

    pub fn get_meta_u64(&self, key: &str) -> Result<Option<u64>, rocksdb::Error> {
        let cf = self.db.cf_handle(CF_META).unwrap();
        match self.db.get_cf(&cf, key.as_bytes())? {
            Some(data) => {
                let mut buf = [0u8; 8];
                buf.copy_from_slice(&data);
                Ok(Some(u64::from_le_bytes(buf)))
            }
            None => Ok(None),
        }
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
            },
            transactions: vec![],
            proof_summaries: vec![],
            compliance_summary: None,
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
