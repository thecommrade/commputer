use crate::block::Block;
use crate::wallet::Wallet;
use crate::transaction::Transaction;
use borsh::BorshSerialize;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};

fn tx_signable_bytes(tx: &Transaction) -> Vec<u8> {
    let mut bytes = Vec::new();
    tx.from.serialize(&mut bytes).unwrap();
    tx.nonce.serialize(&mut bytes).unwrap();
    tx.kind.serialize(&mut bytes).unwrap();
    tx.fee.serialize(&mut bytes).unwrap();
    bytes
}

/// Produce signable bytes for a transaction that include a chain_id,
/// preventing replay attacks across different chains/networks.
pub fn tx_signable_bytes_with_chain_id(tx: &Transaction, chain_id: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    // Prefix with chain_id so signatures are chain-specific
    chain_id.serialize(&mut bytes).unwrap();
    tx.from.serialize(&mut bytes).unwrap();
    tx.nonce.serialize(&mut bytes).unwrap();
    tx.kind.serialize(&mut bytes).unwrap();
    tx.fee.serialize(&mut bytes).unwrap();
    bytes
}

/// Sign a transaction with the sender's wallet key.
pub fn sign_transaction(tx: &mut Transaction, wallet: &Wallet) {
    let bytes = tx_signable_bytes(tx);
    let sig = wallet.sign(&bytes);
    tx.signature = sig.to_bytes().to_vec();
    tx.public_key = wallet.public_key().to_bytes().to_vec();
}

/// Sign a block header with the producer's wallet key.
pub fn sign_block(block: &mut Block, wallet: &Wallet) {
    block.header.producer_public_key = wallet.public_key().to_bytes().to_vec();
    let bytes = block.header.signable_bytes();
    let sig = wallet.sign(&bytes);
    block.header.signature = sig.to_bytes().to_vec();
}

/// Verify a transaction's signature against the given public key.
pub fn verify_transaction(tx: &Transaction, public_key: &VerifyingKey) -> bool {
    if tx.signature.len() != 64 {
        return false;
    }
    let bytes = tx_signable_bytes(tx);
    let sig_bytes: &[u8; 64] = match tx.signature.as_slice().try_into() {
        Ok(b) => b,
        Err(_) => return false,
    };
    let sig = Signature::from_bytes(sig_bytes);
    public_key.verify(&bytes, &sig).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wallet::Wallet;
    use crate::transaction::{Transaction, TxKind};
    use crate::token::Amount;
    use crate::identity::Address;

    #[test]
    fn sign_and_verify_transfer() {
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
        assert!(!tx.signature.is_empty());
        assert!(verify_transaction(&tx, sender.public_key()));
    }

    #[test]
    fn sign_and_verify_block() {
        use crate::block::{Block, BlockHeader, BlockHash};

        let producer = Wallet::generate();
        let mut block = Block {
            header: BlockHeader {
                protocol_version: 1, height: 1,
                parent_hash: BlockHash::GENESIS,
                tx_root: [0u8; 32],
                proof_root: [0u8; 32],
                state_root: [0u8; 32],
                timestamp: 1_700_000_000,
                producer: *producer.address(),
                epoch: 0,
                producer_public_key: vec![],
                signature: vec![],
                checkpoint_hash: None,
                chain_id: "test".to_string(),
            },
            transactions: vec![],
            proof_summaries: vec![],
            compliance_summary: None, epoch_summary: None,
        };

        sign_block(&mut block, &producer);
        assert_eq!(block.header.signature.len(), 64);
        assert!(block.header.verify_signature(&producer.public_key().to_bytes()));
    }

    #[test]
    fn tampered_block_fails_verification() {
        use crate::block::{Block, BlockHeader, BlockHash};

        let producer = Wallet::generate();
        let mut block = Block {
            header: BlockHeader {
                protocol_version: 1, height: 1,
                parent_hash: BlockHash::GENESIS,
                tx_root: [0u8; 32],
                proof_root: [0u8; 32],
                state_root: [0u8; 32],
                timestamp: 1_700_000_000,
                producer: *producer.address(),
                epoch: 0,
                producer_public_key: vec![],
                signature: vec![],
                checkpoint_hash: None,
                chain_id: "test".to_string(),
            },
            transactions: vec![],
            proof_summaries: vec![],
            compliance_summary: None, epoch_summary: None,
        };

        sign_block(&mut block, &producer);
        // Tamper with the header.
        block.header.timestamp = 999;
        assert!(!block.header.verify_signature(&producer.public_key().to_bytes()));
    }

    #[test]
    fn wrong_key_fails_block_verification() {
        use crate::block::{Block, BlockHeader, BlockHash};

        let producer = Wallet::generate();
        let imposter = Wallet::generate();
        let mut block = Block {
            header: BlockHeader {
                protocol_version: 1, height: 1,
                parent_hash: BlockHash::GENESIS,
                tx_root: [0u8; 32],
                proof_root: [0u8; 32],
                state_root: [0u8; 32],
                timestamp: 1_700_000_000,
                producer: *producer.address(),
                epoch: 0,
                producer_public_key: vec![],
                signature: vec![],
                checkpoint_hash: None,
                chain_id: "test".to_string(),
            },
            transactions: vec![],
            proof_summaries: vec![],
            compliance_summary: None, epoch_summary: None,
        };

        // Sign with imposter's key — should fail because key doesn't match producer address.
        sign_block(&mut block, &imposter);
        assert!(!block.header.verify_signature(&imposter.public_key().to_bytes()));
    }

    #[test]
    fn different_chain_ids_produce_different_bytes() {
        let sender = Wallet::generate();
        let recipient = Address([1u8; 32]);
        let tx = Transaction {
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
        let bytes_mainnet = super::tx_signable_bytes_with_chain_id(&tx, "commputer-mainnet");
        let bytes_testnet = super::tx_signable_bytes_with_chain_id(&tx, "commputer-testnet");
        let bytes_no_chain = super::tx_signable_bytes(&tx);
        assert_ne!(bytes_mainnet, bytes_testnet);
        assert_ne!(bytes_mainnet, bytes_no_chain);
    }

    #[test]
    fn tampered_tx_fails_verification() {
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
        tx.nonce = 999;
        assert!(!verify_transaction(&tx, sender.public_key()));
    }
}
