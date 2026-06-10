//! Tier B (B-6) — wallet property/harness tests.
//!
//! Property-based coverage of the Ed25519 + BIP39 wallet beyond the unit tests
//! in `wallet.rs`: sign/verify roundtrips over arbitrary messages, tamper- and
//! cross-wallet rejection, and seed-phrase recovery preserving identity.
//!
//! New file, zero runtime behavior change. (Roadmap: src/staging/docs/wirein_roadmap.md B-6.)

use commputer_core::wallet::Wallet;
use proptest::prelude::*;

proptest! {
    /// A signature made by a wallet verifies under that same wallet for ANY message.
    #[test]
    fn sign_verify_roundtrip(msg in proptest::collection::vec(any::<u8>(), 0..512)) {
        let w = Wallet::generate();
        let sig = w.sign(&msg);
        prop_assert!(w.verify(&msg, &sig), "valid signature must verify");
    }

    /// Flipping a single bit of the message breaks verification.
    #[test]
    fn tampered_message_fails(
        msg in proptest::collection::vec(any::<u8>(), 1..256),
        flip in 0usize..2048,
    ) {
        let w = Wallet::generate();
        let sig = w.sign(&msg);
        let mut tampered = msg.clone();
        let idx = flip % tampered.len();
        tampered[idx] ^= 1; // guaranteed to differ from `msg`
        prop_assert!(!w.verify(&tampered, &sig), "tampered message must not verify");
    }

    /// A signature from wallet A never verifies under wallet B's key.
    #[test]
    fn cross_wallet_signature_rejected(msg in proptest::collection::vec(any::<u8>(), 0..256)) {
        let a = Wallet::generate();
        let b = Wallet::generate();
        let sig = a.sign(&msg);
        // Distinct keypairs (collision is astronomically unlikely); A's sig is
        // invalid under B.
        prop_assert!(!b.verify(&msg, &sig), "foreign signature must be rejected");
    }
}

/// generate → seed_phrase → from_seed_phrase recovers the SAME identity (address,
/// public key) and the recovered wallet can verify the original's signatures.
#[test]
fn seed_phrase_roundtrip_preserves_identity() {
    for _ in 0..50 {
        let w = Wallet::generate();
        let phrase = w.seed_phrase();
        assert_eq!(phrase.split_whitespace().count(), 24, "must be a 24-word mnemonic");

        let r = Wallet::from_seed_phrase(&phrase).expect("own phrase must recover");
        assert_eq!(w.address(), r.address(), "recovered address must match");
        assert_eq!(
            w.public_key().to_bytes(),
            r.public_key().to_bytes(),
            "recovered public key must match"
        );

        let msg = b"roundtrip-identity";
        assert!(
            r.verify(msg, &w.sign(msg)),
            "recovered wallet must verify the original's signature"
        );
    }
}

/// Fresh wallets get distinct addresses (no accidental key reuse).
#[test]
fn distinct_wallets_have_distinct_addresses() {
    use std::collections::HashSet;
    let mut seen: HashSet<[u8; 32]> = HashSet::new();
    for _ in 0..256 {
        let w = Wallet::generate();
        assert!(
            seen.insert(w.address().0),
            "address collision — astronomically unlikely, indicates broken RNG"
        );
    }
}

/// Malformed seed phrases are rejected rather than silently producing a wallet.
#[test]
fn malformed_seed_phrases_rejected() {
    for bad in [
        "",                                   // empty
        "not a valid seed phrase",            // not BIP39 words
        "abandon abandon abandon",            // too few words / bad checksum
        "zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo", // 12 words (wrong length / checksum)
    ] {
        assert!(
            Wallet::from_seed_phrase(bad).is_err(),
            "malformed phrase {bad:?} must be rejected"
        );
    }
}
