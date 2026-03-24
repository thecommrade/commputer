use commputer_core::block::{Block, BlockHash};
use commputer_core::identity::Address;
use crate::account::Account;

/// Abstract storage backend. Implementations can use RocksDB, sled, or in-memory.
/// Starting with in-memory for testing; swappable to persistent later.
pub trait Storage: Send + Sync {
    type Error: std::error::Error;

    // Block operations
    fn put_block(&mut self, block: &Block) -> Result<(), Self::Error>;
    fn get_block(&self, hash: &BlockHash) -> Result<Option<Block>, Self::Error>;
    fn get_block_by_height(&self, height: u64) -> Result<Option<Block>, Self::Error>;
    fn latest_block(&self) -> Result<Option<Block>, Self::Error>;
    fn chain_height(&self) -> Result<u64, Self::Error>;

    // Account operations
    fn get_account(&self, address: &Address) -> Result<Option<Account>, Self::Error>;
    fn put_account(&mut self, account: &Account) -> Result<(), Self::Error>;

    // Burn tracking
    fn total_burned(&self) -> Result<u64, Self::Error>;
    fn record_burn(&mut self, amount: u64) -> Result<(), Self::Error>;

    // Total emitted
    fn total_emitted(&self) -> Result<u64, Self::Error>;
    fn record_emission(&mut self, amount: u64) -> Result<(), Self::Error>;
}
