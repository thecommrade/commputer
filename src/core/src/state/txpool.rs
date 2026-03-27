use std::sync::RwLock;

use crate::transaction::{Transaction, TxHash};

/// In-memory pool of pending transactions awaiting inclusion in a block.
pub struct TransactionPool {
    pending: RwLock<Vec<Transaction>>,
}

impl TransactionPool {
    /// Create an empty transaction pool.
    pub fn new() -> Self {
        Self {
            pending: RwLock::new(Vec::new()),
        }
    }

    /// Add a transaction to the pool.
    pub fn add(&self, tx: Transaction) {
        let mut pool = self.pending.write().expect("txpool lock poisoned");
        pool.push(tx);
    }

    /// Return up to `limit` pending transactions, ordered by nonce ascending.
    pub fn get_pending(&self, limit: usize) -> Vec<Transaction> {
        let pool = self.pending.read().expect("txpool lock poisoned");
        let mut txs: Vec<Transaction> = pool.clone();
        txs.sort_by_key(|tx| tx.nonce);
        txs.truncate(limit);
        txs
    }

    /// Remove the transaction with the given hash from the pool.
    pub fn remove(&self, tx_hash: &TxHash) {
        let mut pool = self.pending.write().expect("txpool lock poisoned");
        pool.retain(|tx| tx.hash() != *tx_hash);
    }

    /// Number of pending transactions in the pool.
    pub fn len(&self) -> usize {
        self.pending.read().expect("txpool lock poisoned").len()
    }

    /// Whether the pool is empty.
    pub fn is_empty(&self) -> bool {
        self.pending.read().expect("txpool lock poisoned").is_empty()
    }
}

impl Default for TransactionPool {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Address;
    use crate::token::Amount;
    use crate::transaction::{Transaction, TxKind};

    fn make_tx(nonce: u64) -> Transaction {
        Transaction {
            from: Address([0u8; 32]),
            nonce,
            fee: 0,
            kind: TxKind::Transfer {
                to: Address([1u8; 32]),
                amount: Amount::from_raw(1000),
            },
            public_key: vec![],
            signature: vec![],
            memo: None,
            timelock: None,
        }
    }

    #[test]
    fn test_add_pending_tx() {
        let pool = TransactionPool::new();
        let tx = make_tx(0);
        let hash = tx.hash();
        pool.add(tx);

        assert_eq!(pool.len(), 1);
        let pending = pool.get_pending(10);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].hash(), hash);
    }

    #[test]
    fn test_get_pending_limit() {
        let pool = TransactionPool::new();
        for i in 0..10 {
            pool.add(make_tx(i));
        }
        assert_eq!(pool.len(), 10);

        let pending = pool.get_pending(5);
        assert_eq!(pending.len(), 5);
    }

    #[test]
    fn test_remove_pending() {
        let pool = TransactionPool::new();
        let tx0 = make_tx(0);
        let tx1 = make_tx(1);
        let tx2 = make_tx(2);
        let hash0 = tx0.hash();
        let hash1 = tx1.hash();
        let hash2 = tx2.hash();

        pool.add(tx0);
        pool.add(tx1);
        pool.add(tx2);
        assert_eq!(pool.len(), 3);

        pool.remove(&hash0);
        pool.remove(&hash1);
        assert_eq!(pool.len(), 1);

        let pending = pool.get_pending(10);
        assert_eq!(pending[0].hash(), hash2);
    }

    #[test]
    fn test_pending_ordering() {
        let pool = TransactionPool::new();
        // Add in reverse nonce order
        pool.add(make_tx(5));
        pool.add(make_tx(1));
        pool.add(make_tx(3));
        pool.add(make_tx(2));
        pool.add(make_tx(4));

        let pending = pool.get_pending(5);
        for i in 0..pending.len() - 1 {
            assert!(
                pending[i].nonce <= pending[i + 1].nonce,
                "expected nonce {} <= {}, but ordering is wrong",
                pending[i].nonce,
                pending[i + 1].nonce
            );
        }
        assert_eq!(pending[0].nonce, 1);
        assert_eq!(pending[4].nonce, 5);
    }
}
