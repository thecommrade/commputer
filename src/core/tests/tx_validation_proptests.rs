//! Tier B (B-2) — transaction validation property tests.
//!
//! Properties of the signed-transaction layer: sign/verify roundtrip over
//! arbitrary (nonce, fee, amount), per-field tamper rejection, key-substitution
//! rejection, structural `validate_shape` invariants, and `burn_amount`.
//!
//! New file, zero runtime behavior change. (Roadmap: src/staging/docs/wirein_roadmap.md B-2.)

use commputer_core::identity::Address;
use commputer_core::proof::ResourceChannel;
use commputer_core::signing::{sign_transaction, verify_transaction};
use commputer_core::token::Amount;
use commputer_core::transaction::{Transaction, TxKind};
use commputer_core::wallet::Wallet;
use proptest::prelude::*;

fn signed_transfer(w: &Wallet, nonce: u64, fee: u64, to: Address, amount_comme: u64) -> Transaction {
    let mut tx = Transaction {
        from: *w.address(),
        nonce,
        kind: TxKind::Transfer { to, amount: Amount::from_comme(amount_comme) },
        fee,
        signature: vec![],
        public_key: vec![],
        memo: None,
        timelock: None,
    };
    sign_transaction(&mut tx, w);
    tx
}

proptest! {
    /// A properly signed transfer verifies (both the inline method and the
    /// signing-module verifier) and passes structural validation.
    #[test]
    fn signed_transfer_verifies(
        nonce in any::<u64>(),
        fee in any::<u64>(),
        amt in 0u64..1_000_000,
        rb in any::<u8>(),
    ) {
        let w = Wallet::generate();
        let tx = signed_transfer(&w, nonce, fee, Address([rb; 32]), amt);
        prop_assert!(tx.verify(), "tx.verify() must accept a correctly signed tx");
        prop_assert!(verify_transaction(&tx, w.public_key()), "verify_transaction must agree");
        prop_assert!(tx.validate_shape().is_ok(), "a real tx has a valid shape");
    }

    /// Mutating the nonce after signing invalidates the signature.
    #[test]
    fn tampering_nonce_breaks_signature(
        nonce in any::<u64>(), fee in any::<u64>(), amt in 0u64..1_000_000, delta in 1u64..1024,
    ) {
        let w = Wallet::generate();
        let mut tx = signed_transfer(&w, nonce, fee, Address([7; 32]), amt);
        tx.nonce = tx.nonce.wrapping_add(delta);
        prop_assert!(!tx.verify(), "nonce tamper must break verification");
    }

    /// Mutating the fee after signing invalidates the signature.
    #[test]
    fn tampering_fee_breaks_signature(
        nonce in any::<u64>(), fee in any::<u64>(), amt in 0u64..1_000_000, delta in 1u64..1024,
    ) {
        let w = Wallet::generate();
        let mut tx = signed_transfer(&w, nonce, fee, Address([7; 32]), amt);
        tx.fee = tx.fee.wrapping_add(delta);
        prop_assert!(!tx.verify(), "fee tamper must break verification");
    }

    /// Mutating the transferred amount after signing invalidates the signature.
    #[test]
    fn tampering_amount_breaks_signature(
        nonce in any::<u64>(), amt in 0u64..1_000_000, delta in 1u64..1024,
    ) {
        let w = Wallet::generate();
        let mut tx = signed_transfer(&w, nonce, 0, Address([7; 32]), amt);
        tx.kind = TxKind::Transfer { to: Address([7; 32]), amount: Amount::from_comme(amt + delta) };
        prop_assert!(!tx.verify(), "amount tamper must break verification");
    }

    /// Swapping in a different wallet's public key is rejected (the embedded key
    /// must hash to `from`, preventing key substitution).
    #[test]
    fn key_substitution_rejected(nonce in any::<u64>(), fee in any::<u64>(), amt in 0u64..1_000_000) {
        let w = Wallet::generate();
        let other = Wallet::generate();
        let mut tx = signed_transfer(&w, nonce, fee, Address([7; 32]), amt);
        tx.public_key = other.public_key().to_bytes().to_vec();
        prop_assert!(!tx.verify(), "public key not matching `from` must be rejected");
    }
}

#[test]
fn validate_shape_rejects_bad_field_lengths() {
    let w = Wallet::generate();
    let ok = signed_transfer(&w, 0, 0, Address([1; 32]), 1);
    assert!(ok.validate_shape().is_ok());

    let mut bad_pk = ok.clone();
    bad_pk.public_key = vec![0u8; 31];
    assert!(bad_pk.validate_shape().is_err(), "31-byte pubkey must be rejected");

    let mut bad_sig = ok.clone();
    bad_sig.signature = vec![0u8; 63];
    assert!(bad_sig.validate_shape().is_err(), "63-byte signature must be rejected");

    let mut big_memo = ok.clone();
    big_memo.memo = Some(vec![0u8; Transaction::MAX_MEMO_LENGTH + 1]);
    assert!(big_memo.validate_shape().is_err(), "oversized memo must be rejected");

    let mut max_memo = ok.clone();
    max_memo.memo = Some(vec![0u8; Transaction::MAX_MEMO_LENGTH]);
    assert!(max_memo.validate_shape().is_ok(), "exactly-max memo is allowed");
}

#[test]
fn validate_shape_rejects_nested_batch() {
    let w = Wallet::generate();
    let inner = TxKind::Batch { operations: vec![] };
    let mut tx = Transaction {
        from: *w.address(),
        nonce: 0,
        kind: TxKind::Batch { operations: vec![inner] },
        fee: 0,
        signature: vec![],
        public_key: vec![],
        memo: None,
        timelock: None,
    };
    sign_transaction(&mut tx, &w);
    assert_eq!(tx.validate_shape(), Err("nested Batch is not allowed"));
}

#[test]
fn burn_amount_matches_kind() {
    let w = Wallet::generate();

    // A transfer burns nothing.
    let transfer = signed_transfer(&w, 0, 0, Address([1; 32]), 5);
    assert_eq!(transfer.burn_amount(), Amount::ZERO);
    assert!(!transfer.is_burn());

    // BurstCompute burns exactly its burn_amount.
    let burn = Amount::from_comme(7);
    let mut burst = Transaction {
        from: *w.address(),
        nonce: 1,
        kind: TxKind::BurstCompute {
            channel: ResourceChannel::Processing,
            burn_amount: burn,
            job_hash: [0u8; 32],
        },
        fee: 0,
        signature: vec![],
        public_key: vec![],
        memo: None,
        timelock: None,
    };
    sign_transaction(&mut burst, &w);
    assert_eq!(burst.burn_amount(), burn);
    assert!(burst.is_burn());
    assert!(burst.verify(), "a burn tx still verifies");
}
