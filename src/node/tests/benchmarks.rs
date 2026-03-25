//! Features 204 and 206: Simple timing benchmarks and stress test.
//! These use std::time::Instant instead of criterion.

use std::time::Instant;
use commputer_core::block::{Block, BlockHeader, BlockHash};
use commputer_core::identity::Address;
use commputer_core::token::Amount;
use commputer_core::transaction::{Transaction, TxKind};
use commputer_core::wallet::Wallet;
use commputer_core::signing::{sign_transaction, sign_block};
use commputer_core::merkle;
use commputer_storage::state::ChainState;

fn addr(n: u8) -> Address {
    let mut a = [0u8; 32];
    a[0] = n;
    Address(a)
}

fn genesis_block() -> Block {
    Block {
        header: BlockHeader {
            protocol_version: 1,
            height: 0,
            parent_hash: BlockHash::GENESIS,
            tx_root: [0u8; 32],
            proof_root: [0u8; 32],
            state_root: [0u8; 32],
            timestamp: 1000,
            producer: addr(0),
            epoch: 0,
            producer_public_key: vec![],
            signature: vec![],
            checkpoint_hash: None, chain_id: "test".to_string(),
        },
        transactions: vec![],
        proof_summaries: vec![],
        compliance_summary: None,
    }
}

// Feature 204: Timing benchmarks
#[test]
#[ignore] // Benchmark — run explicitly
fn feature_204_benchmarks() {
    // Benchmark 1: Block validation (100 iterations)
    {
        let wallet = Wallet::generate();
        let mut block = Block {
            header: BlockHeader {
                protocol_version: 1,
                height: 1,
                parent_hash: BlockHash::GENESIS,
                tx_root: [0u8; 32],
                proof_root: [0u8; 32],
                state_root: [0u8; 32],
                timestamp: 1_700_000_000,
                producer: *wallet.address(),
                epoch: 0,
                producer_public_key: vec![],
                signature: vec![],
                checkpoint_hash: None, chain_id: "test".to_string(),
            },
            transactions: vec![],
            proof_summaries: vec![],
            compliance_summary: None,
        };
        sign_block(&mut block, &wallet);

        let start = Instant::now();
        for _ in 0..100 {
            let _ = block.verify_producer_signature();
            let _ = block.verify_roots();
            let _ = block.within_size_limits();
        }
        let elapsed = start.elapsed();
        eprintln!(
            "Block validation (100 iter): {:?} ({:.0} us/iter)",
            elapsed,
            elapsed.as_micros() as f64 / 100.0
        );
    }

    // Benchmark 2: Signature verification (100 iterations)
    {
        let wallet = Wallet::generate();
        let mut tx = Transaction {
            from: *wallet.address(),
            nonce: 0,
            kind: TxKind::Transfer {
                to: addr(2),
                amount: Amount::from_comme(10),
            },
            fee: 100_000,
            signature: vec![],
            public_key: vec![],
            memo: None,
            timelock: None,
        };
        sign_transaction(&mut tx, &wallet);

        let start = Instant::now();
        for _ in 0..100 {
            let _ = tx.verify();
        }
        let elapsed = start.elapsed();
        eprintln!(
            "Signature verification (100 iter): {:?} ({:.0} us/iter)",
            elapsed,
            elapsed.as_micros() as f64 / 100.0
        );
    }

    // Benchmark 3: Merkle root computation
    {
        let leaves: Vec<[u8; 32]> = (0..500)
            .map(|i| {
                let mut out = [0u8; 32];
                let bytes = format!("tx{}", i);
                let b = bytes.as_bytes();
                for (j, byte) in b.iter().enumerate() {
                    if j < 32 {
                        out[j] = *byte;
                    }
                }
                out
            })
            .collect();

        let start = Instant::now();
        for _ in 0..100 {
            let _ = merkle::merkle_root(&leaves);
        }
        let elapsed = start.elapsed();
        eprintln!(
            "Merkle root 500 leaves (100 iter): {:?} ({:.0} us/iter)",
            elapsed,
            elapsed.as_micros() as f64 / 100.0
        );
    }

    // Benchmark 4: Block apply (100 empty blocks)
    {
        let mut state = ChainState::new();
        state.apply_block(&genesis_block()).unwrap();

        let start = Instant::now();
        for h in 1..=100u64 {
            let parent = state.blocks.latest().unwrap().hash();
            let block = Block {
                header: BlockHeader {
                    protocol_version: 1,
                    height: h,
                    parent_hash: parent,
                    tx_root: [0u8; 32],
                    proof_root: [0u8; 32],
                    state_root: [0u8; 32],
                    timestamp: 1000 + h * 10,
                    producer: addr(0),
                    epoch: 0,
                    producer_public_key: vec![],
                    signature: vec![],
                    checkpoint_hash: None, chain_id: "test".to_string(),
                },
                transactions: vec![],
                proof_summaries: vec![],
                compliance_summary: None,
            };
            state.apply_block(&block).unwrap();
        }
        let elapsed = start.elapsed();
        eprintln!(
            "Apply 100 empty blocks: {:?} ({:.0} us/block)",
            elapsed,
            elapsed.as_micros() as f64 / 100.0
        );
    }
}

// Feature 206: Stress test — generate many transactions and measure throughput
#[test]
#[ignore] // Stress test — run explicitly
fn feature_206_stress_test_transactions() {
    let mut state = ChainState::new();
    state.apply_block(&genesis_block()).unwrap();

    // Fund a sender
    let acct = state.accounts.get_or_create(addr(1));
    acct.balance = Amount::from_comme(1_000_000);
    state.total_emitted = Amount::from_comme(1_000_000).raw();

    let num_txs = 1000;
    let mut transactions = Vec::with_capacity(num_txs);

    // Create many transfer transactions
    for i in 0..num_txs as u64 {
        let tx = Transaction {
            from: addr(1),
            nonce: i,
            kind: TxKind::Transfer {
                to: addr(2),
                amount: Amount::from_raw(1),
            },
            fee: 0,
            signature: vec![],
            public_key: vec![],
            memo: None,
            timelock: None,
        };
        transactions.push(tx);
    }

    // Process transactions in blocks of 100
    let start = Instant::now();
    let mut block_height = 1u64;
    for chunk in transactions.chunks(100) {
        let parent = state.blocks.latest().unwrap().hash();
        let block = Block {
            header: BlockHeader {
                protocol_version: 1,
                height: block_height,
                parent_hash: parent,
                tx_root: [0u8; 32],
                proof_root: [0u8; 32],
                state_root: [0u8; 32],
                timestamp: 1000 + block_height * 10,
                producer: addr(0),
                epoch: 0,
                producer_public_key: vec![],
                signature: vec![],
                checkpoint_hash: None, chain_id: "test".to_string(),
            },
            transactions: chunk.to_vec(),
            proof_summaries: vec![],
            compliance_summary: None,
        };
        state.apply_block(&block).unwrap();
        block_height += 1;
    }
    let elapsed = start.elapsed();

    let tps = num_txs as f64 / elapsed.as_secs_f64();
    eprintln!(
        "Feature 206: {} transactions in {:?} ({:.0} tx/sec)",
        num_txs, elapsed, tps
    );

    assert!(tps > 100.0, "Throughput should be at least 100 tx/sec, got {:.0}", tps);
}
