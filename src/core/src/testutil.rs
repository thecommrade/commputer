//! Shared test utilities for creating wallets, blocks, transactions, and proofs.
//! Available to all crates in the workspace via `commputer_core::testutil`.

use crate::block::{Block, BlockHeader, BlockHash};
use crate::identity::Address;
use crate::token::Amount;
use crate::transaction::{Transaction, TxKind};
use crate::wallet::Wallet;
use crate::signing::sign_transaction;

/// Create a deterministic test address from a single byte.
pub fn test_addr(n: u8) -> Address {
    let mut a = [0u8; 32];
    a[0] = n;
    Address(a)
}

/// Create a minimal test block at the given height.
pub fn test_block(height: u64) -> Block {
    test_block_with_parent(height, BlockHash::GENESIS)
}

/// Create a test block with a specific parent hash.
pub fn test_block_with_parent(height: u64, parent: BlockHash) -> Block {
    Block {
        header: BlockHeader {
            protocol_version: 1,
            height,
            parent_hash: parent,
            tx_root: [0u8; 32],
            proof_root: [0u8; 32],
            state_root: [0u8; 32],
            timestamp: 1_700_000_000 + height,
            producer: test_addr(0),
            epoch: 0,
            producer_public_key: vec![],
            signature: vec![],
            checkpoint_hash: None,
                chain_id: String::new(),
        },
        transactions: vec![],
        proof_summaries: vec![],
        compliance_summary: None,
            epoch_summary: None,
    }
}

/// Create a test block with a specific producer.
pub fn test_block_with_producer(height: u64, producer: Address) -> Block {
    Block {
        header: BlockHeader {
            protocol_version: 1,
            height,
            parent_hash: BlockHash::GENESIS,
            tx_root: [0u8; 32],
            proof_root: [0u8; 32],
            state_root: [0u8; 32],
            timestamp: 1_700_000_000 + height,
            producer,
            epoch: 0,
            producer_public_key: vec![],
            signature: vec![],
            checkpoint_hash: None,
                chain_id: String::new(),
        },
        transactions: vec![],
        proof_summaries: vec![],
        compliance_summary: None,
            epoch_summary: None,
    }
}

/// Create a signed test transfer transaction.
pub fn signed_transfer(wallet: &Wallet, to: Address, amount: u64, nonce: u64) -> Transaction {
    let mut tx = Transaction {
        from: *wallet.address(),
        nonce,
        kind: TxKind::Transfer {
            to,
            amount: Amount::from_comme(amount),
        },
        fee: crate::transaction::MINIMUM_FEE,
        signature: vec![],
        public_key: vec![],
        memo: None,
        timelock: None,
    };
    sign_transaction(&mut tx, wallet);
    tx
}

/// Create a genesis block (height 0, null producer).
pub fn genesis_block() -> Block {
    Block {
        header: BlockHeader {
            protocol_version: 1, height: 0,
            parent_hash: BlockHash::GENESIS,
            tx_root: [0u8; 32],
            proof_root: [0u8; 32],
            state_root: [0u8; 32],
            timestamp: 0,
            producer: Address([0u8; 32]),
            epoch: 0,
            producer_public_key: vec![],
            signature: vec![],
            checkpoint_hash: None,
                chain_id: String::new(),
        },
        transactions: vec![],
        proof_summaries: vec![],
        compliance_summary: None,
            epoch_summary: None,
    }
}
