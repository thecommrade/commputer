#![allow(dead_code)]
use commputer_core::transaction::Transaction;
use rayon::prelude::*;

/// Validate a slice of transactions in parallel using rayon.
/// Returns a Vec<bool> where each element indicates whether the
/// corresponding transaction has a valid signature.
pub fn validate_transactions_parallel(txs: &[Transaction]) -> Vec<bool> {
    txs.par_iter().map(|tx| tx.verify()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use commputer_core::wallet::Wallet;
    use commputer_core::transaction::{Transaction, TxKind};
    use commputer_core::token::Amount;
    use commputer_core::identity::Address;
    use commputer_core::signing::sign_transaction;

    fn make_signed_tx() -> Transaction {
        let sender = Wallet::generate();
        let recipient = Address([1u8; 32]);
        let mut tx = Transaction {
            from: *sender.address(),
            nonce: 0,
            kind: TxKind::Transfer {
                to: recipient,
                amount: Amount::from_comme(10),
            },
            fee: 0,
            signature: vec![],
            public_key: vec![],
            memo: None,
            timelock: None,
        };
        sign_transaction(&mut tx, &sender);
        tx
    }

    #[test]
    fn parallel_validation_all_valid() {
        let txs: Vec<Transaction> = (0..10).map(|_| make_signed_tx()).collect();
        let results = validate_transactions_parallel(&txs);
        assert_eq!(results.len(), 10);
        assert!(results.iter().all(|&v| v));
    }

    #[test]
    fn parallel_validation_detects_invalid() {
        let mut txs: Vec<Transaction> = (0..5).map(|_| make_signed_tx()).collect();
        // Tamper with the third transaction
        txs[2].nonce = 999;
        let results = validate_transactions_parallel(&txs);
        assert!(results[0]);
        assert!(results[1]);
        assert!(!results[2]);
        assert!(results[3]);
        assert!(results[4]);
    }

    #[test]
    fn parallel_validation_empty() {
        let results = validate_transactions_parallel(&[]);
        assert!(results.is_empty());
    }
}
