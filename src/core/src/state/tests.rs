//! Task 12: Final integration test — multi-block lifecycle end-to-end.

use super::*;
use crate::block::{BlockHeader, CURRENT_PROTOCOL_VERSION};
use crate::genesis::default_genesis;
use crate::identity::Address;
use crate::proof::ResourceChannel;
use crate::testutil::test_addr;
use crate::token::{Amount, UNITS_PER_COMME};
use crate::transaction::{Transaction, TxKind};

fn make_block(height: u64, parent_hash: BlockHash, txs: Vec<Transaction>) -> Block {
    Block {
        header: BlockHeader {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            height,
            parent_hash,
            tx_root: [0u8; 32],
            proof_root: [0u8; 32],
            state_root: [0u8; 32],
            timestamp: 1000000 + height,
            producer: Address([0u8; 32]),
            epoch: 0,
            producer_public_key: vec![],
            signature: vec![],
            checkpoint_hash: None,
            chain_id: "commputer-testnet-1".to_string(),
        },
        transactions: txs,
        proof_summaries: vec![],
        compliance_summary: None,
        epoch_summary: None,
    }
}

#[test]
fn test_multi_block_lifecycle() {
    // -- Setup --
    let store = InMemoryStore::new();
    let manager = StateManager::new(store);
    let config = default_genesis();

    // a. Init genesis
    manager.init_genesis(&config).unwrap();

    let alice = test_addr(1);
    let bob = test_addr(2);

    // b. Fund alice with 100 COMME, fund bob with 50 COMME
    let alice_initial = 100 * UNITS_PER_COMME;
    let bob_initial = 50 * UNITS_PER_COMME;
    manager.fund_account(&alice, alice_initial).unwrap();
    manager.fund_account(&bob, bob_initial).unwrap();

    // c. Register alice as a validator
    let signing_key = ed25519_dalek::SigningKey::from_bytes(&test_addr(1).0);
    let public_key = signing_key.verifying_key().to_bytes().to_vec();

    let register_tx = Transaction {
        from: alice,
        nonce: 0,
        fee: 0,
        kind: TxKind::ValidatorRegister {
            hardware_fingerprint_hash: [0u8; 32],
            contribution_percent: 100,
        },
        public_key: public_key.clone(),
        signature: vec![],
        memo: None,
        timelock: None,
    };

    // d. Block 1: validator register + alice transfers 20 COMME to bob
    let transfer_amount = 20 * UNITS_PER_COMME;
    let transfer_tx = Transaction {
        from: alice,
        nonce: 0,
        fee: 0,
        kind: TxKind::Transfer {
            to: bob,
            amount: Amount::from_raw(transfer_amount),
        },
        public_key: vec![],
        signature: vec![],
        memo: None,
        timelock: None,
    };

    let (_, genesis_hash) = manager.store().get_chain_tip().unwrap().unwrap();
    let block1 = make_block(1, genesis_hash, vec![register_tx, transfer_tx]);
    manager.apply_block(&block1).unwrap();

    // e. Block 2: bob burns 5 COMME for burst compute
    let burn_amount = 5 * UNITS_PER_COMME;
    let burn_tx = Transaction {
        from: bob,
        nonce: 0,
        fee: 0,
        kind: TxKind::BurstCompute {
            channel: ResourceChannel::Processing,
            burn_amount: Amount::from_raw(burn_amount),
            job_hash: [0u8; 32],
        },
        public_key: vec![],
        signature: vec![],
        memo: None,
        timelock: None,
    };

    let (_, block1_hash) = manager.store().get_chain_tip().unwrap().unwrap();
    let block2 = make_block(2, block1_hash, vec![burn_tx]);
    manager.apply_block(&block2).unwrap();

    // -- f. Verify all state after block 2 --

    // alice balance = 80 COMME (100 - 20 transfer)
    let alice_rec = manager.store().get_account(&alice).unwrap().unwrap();
    assert_eq!(
        alice_rec.balance,
        80 * UNITS_PER_COMME,
        "alice balance should be 80 COMME"
    );

    // bob balance = 65 COMME (50 + 20 - 5)
    let bob_rec = manager.store().get_account(&bob).unwrap().unwrap();
    assert_eq!(
        bob_rec.balance,
        65 * UNITS_PER_COMME,
        "bob balance should be 65 COMME"
    );

    // alice nonce = 1 (one transfer)
    assert_eq!(alice_rec.nonce, 1, "alice nonce should be 1");

    // bob nonce = 1 (one burn)
    assert_eq!(bob_rec.nonce, 1, "bob nonce should be 1");

    // emission total_burned = 5 COMME
    let emission = manager.emission();
    assert_eq!(
        emission.total_burned,
        5 * UNITS_PER_COMME,
        "total burned should be 5 COMME"
    );
    drop(emission);

    // alice is in validator registry
    let alice_validator = manager.store().get_validator(&alice).unwrap();
    assert!(
        alice_validator.is_some(),
        "alice should be registered as a validator"
    );

    // chain tip = height 2
    let (tip_height, _tip_hash) = manager.store().get_chain_tip().unwrap().unwrap();
    assert_eq!(tip_height, 2, "chain tip should be at height 2");

    // state root is deterministic (compute it twice, same result)
    let root1 = manager.compute_state_root().unwrap();
    let root2 = manager.compute_state_root().unwrap();
    assert_eq!(
        root1, root2,
        "state root must be deterministic across calls"
    );
    assert_ne!(root1, [0u8; 32], "state root should not be all zeros");
}
