use std::collections::HashMap;
use commputer_core::block::{Block, BlockHash};

/// In-memory block store. Will be backed by RocksDB in production.
#[derive(Debug, Default)]
pub struct BlockStore {
    blocks: HashMap<BlockHash, Block>,
    height_index: HashMap<u64, BlockHash>,
    latest_height: u64,
}

impl BlockStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn put(&mut self, block: Block) {
        let hash = block.hash();
        let height = block.height();
        if height > self.latest_height || (height == 0 && self.blocks.is_empty()) {
            self.latest_height = height;
        }
        self.height_index.insert(height, hash);
        self.blocks.insert(hash, block);
    }

    pub fn get(&self, hash: &BlockHash) -> Option<&Block> {
        self.blocks.get(hash)
    }

    pub fn get_by_height(&self, height: u64) -> Option<&Block> {
        self.height_index
            .get(&height)
            .and_then(|hash| self.blocks.get(hash))
    }

    pub fn latest(&self) -> Option<&Block> {
        if self.blocks.is_empty() {
            None
        } else {
            self.get_by_height(self.latest_height)
        }
    }

    pub fn height(&self) -> u64 {
        self.latest_height
    }

    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    pub fn contains(&self, hash: &BlockHash) -> bool {
        self.blocks.contains_key(hash)
    }

    /// Remove blocks from memory that are older than `keep_last` heights.
    /// The latest `keep_last` blocks are retained. Returns the number of blocks pruned.
    /// Note: pruned blocks should still be available in RocksDB.
    pub fn prune(&mut self, keep_last: u64) -> usize {
        if self.latest_height < keep_last || self.blocks.len() <= keep_last as usize {
            return 0;
        }

        let cutoff = self.latest_height - keep_last;
        let heights_to_remove: Vec<u64> = self.height_index.keys()
            .filter(|&&h| h < cutoff)
            .copied()
            .collect();

        let mut pruned = 0;
        for h in heights_to_remove {
            if let Some(hash) = self.height_index.remove(&h) {
                self.blocks.remove(&hash);
                pruned += 1;
            }
        }
        pruned
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use commputer_core::block::{Block, BlockHeader};
    use commputer_core::identity::Address;

    fn make_block(height: u64, parent: BlockHash) -> Block {
        Block {
            header: BlockHeader {
                height,
                parent_hash: parent,
                tx_root: [0u8; 32],
                proof_root: [0u8; 32],
                state_root: [0u8; 32],
                timestamp: 1000 + height,
                producer: Address([0u8; 32]),
                epoch: 0,
                signature: vec![],
            },
            transactions: vec![],
            proof_summaries: vec![],
        }
    }

    #[test]
    fn store_and_retrieve() {
        let mut store = BlockStore::new();
        let genesis = make_block(0, BlockHash::GENESIS);
        let genesis_hash = genesis.hash();
        store.put(genesis);

        assert_eq!(store.height(), 0);
        assert!(store.get(&genesis_hash).is_some());
        assert!(store.get_by_height(0).is_some());
        assert!(store.latest().is_some());
    }

    #[test]
    fn prune_removes_old_blocks() {
        let mut store = BlockStore::new();
        let b0 = make_block(0, BlockHash::GENESIS);
        let h0 = b0.hash();
        store.put(b0);

        let b1 = make_block(1, h0);
        let h1 = b1.hash();
        store.put(b1);

        let b2 = make_block(2, h1);
        let h2 = b2.hash();
        store.put(b2);

        let b3 = make_block(3, h2);
        let h3 = b3.hash();
        store.put(b3);

        assert_eq!(store.len(), 4);

        // Keep only last 2 blocks. cutoff = 3 - 2 = 1, remove heights < 1.
        let pruned = store.prune(2);
        assert_eq!(pruned, 1); // only block 0 removed
        assert_eq!(store.len(), 3);

        // Prune again — no change since blocks 1,2,3 are within window.
        let pruned2 = store.prune(2);
        assert_eq!(pruned2, 0);

        // Keep only 1 block. cutoff = 3 - 1 = 2, remove heights < 2.
        let pruned3 = store.prune(1);
        assert_eq!(pruned3, 1); // only block 1 removed (block 0 already gone)
        assert_eq!(store.len(), 2); // blocks 2 and 3 remain

        // Blocks 2 and 3 remain; 0 and 1 are gone.
        assert!(store.get_by_height(3).is_some());
        assert!(store.get_by_height(2).is_some());
        assert!(store.get_by_height(1).is_none());
        assert!(store.get_by_height(0).is_none());
        assert_eq!(store.height(), 3);
    }

    #[test]
    fn prune_no_op_when_few_blocks() {
        let mut store = BlockStore::new();
        let b0 = make_block(0, BlockHash::GENESIS);
        store.put(b0);
        let pruned = store.prune(100);
        assert_eq!(pruned, 0);
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn height_tracking() {
        let mut store = BlockStore::new();
        let b0 = make_block(0, BlockHash::GENESIS);
        let h0 = b0.hash();
        store.put(b0);

        let b1 = make_block(1, h0);
        let h1 = b1.hash();
        store.put(b1);

        let b2 = make_block(2, h1);
        store.put(b2);

        assert_eq!(store.height(), 2);
        assert_eq!(store.len(), 3);
    }
}
