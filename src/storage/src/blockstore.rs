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
