use serde::{Deserialize, Serialize};
use commputer_core::transaction::TxHash;
use commputer_core::block::BlockHash;
use std::collections::HashMap;

/// A transaction receipt — proof that a tx was included in a block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxReceipt {
    pub tx_hash: TxHash,
    pub block_hash: BlockHash,
    pub block_height: u64,
    pub tx_index: usize,
    pub success: bool,
}

/// In-memory receipt store. Indexed by transaction hash.
#[derive(Debug, Default)]
pub struct ReceiptStore {
    receipts: HashMap<TxHash, TxReceipt>,
}

impl ReceiptStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, receipt: TxReceipt) {
        self.receipts.insert(receipt.tx_hash, receipt);
    }

    pub fn get(&self, tx_hash: &TxHash) -> Option<&TxReceipt> {
        self.receipts.get(tx_hash)
    }

    pub fn len(&self) -> usize {
        self.receipts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.receipts.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use commputer_core::block::BlockHash;

    #[test]
    fn insert_and_retrieve_receipt() {
        let mut store = ReceiptStore::new();
        let tx_hash = TxHash([1u8; 32]);
        let receipt = TxReceipt {
            tx_hash,
            block_hash: BlockHash([2u8; 32]),
            block_height: 42,
            tx_index: 0,
            success: true,
        };
        store.insert(receipt);
        let loaded = store.get(&tx_hash).unwrap();
        assert_eq!(loaded.block_height, 42);
        assert!(loaded.success);
    }
}
