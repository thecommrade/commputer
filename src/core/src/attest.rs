//! Peer -> validator attestation primitive (QC-009 vote-path gate).
//!
//! A node proves control of a validator address to a peer by signing, with its
//! validator wallet key, a message that binds the CHALLENGER's peer id, the
//! RESPONDER's own peer id, the chain id, and a fresh nonce. The challenger
//! verifies the signature and derives the responder's Address from the public
//! key. The binding lets vote intake count only peers that proved control of an
//! eligible validator, closing QC-009 without which the Stage-2 rung clamp is a
//! fork/capture vector.
//!
//! ANTI-RELAY: the signed bytes include BOTH peer ids, so an attestation copied
//! off the wire is worthless to a different PeerId — it verifies only for the
//! exact (responder_peer, challenger_peer) pair it was made for. A relayed
//! attestation binds nothing for the relayer.
//!
//! DOMAIN SEPARATION: the responder signs with the SAME wallet key it signs
//! blocks and transactions with, so the attest message is prefixed with a domain
//! tag that cannot collide with a block's borsh header (block.rs `signable_bytes`)
//! or a transaction's bytes — a signature can never be cross-protocol replayed.
//!
//! This crate has no libp2p dependency, so peer ids cross the boundary as opaque
//! `&[u8]` (the caller passes `PeerId::to_bytes()`); the primitive never
//! interprets them, it only binds them into the signed message.

use ed25519_dalek::{Signature, Verifier, VerifyingKey};

use crate::identity::Address;

/// Domain tag prefixing every attest message. Distinct from block/tx signable
/// bytes so a validator signature is never valid across protocols.
pub const ATTEST_DOMAIN: &[u8] = b"commputer/attest/1";

fn push_len_prefixed(out: &mut Vec<u8>, field: &[u8]) {
    out.extend_from_slice(&(field.len() as u32).to_le_bytes());
    out.extend_from_slice(field);
}

/// The exact bytes a responder signs (and a challenger verifies). SIGNER AND
/// VERIFIER MUST CALL THIS SAME FUNCTION — never hand-assemble either side.
///
/// Layout: DOMAIN ‖ len(chain_id)‖chain_id ‖ len(responder)‖responder ‖
/// len(challenger)‖challenger ‖ nonce. Length prefixes make the concatenation
/// unambiguous (no field boundary can be shifted).
pub fn build_attest_bytes(
    chain_id: &str,
    responder_peer: &[u8],
    challenger_peer: &[u8],
    nonce: &[u8; 32],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        ATTEST_DOMAIN.len() + 12 + chain_id.len() + responder_peer.len() + challenger_peer.len() + 32,
    );
    out.extend_from_slice(ATTEST_DOMAIN);
    push_len_prefixed(&mut out, chain_id.as_bytes());
    push_len_prefixed(&mut out, responder_peer);
    push_len_prefixed(&mut out, challenger_peer);
    out.extend_from_slice(nonce);
    out
}

/// Verify an attestation and, on success, return the responder's validator
/// Address (SHA-256 of the attested public key, matching `Address::from_public_key`
/// and the block producer-signature discipline). Returns `None` on any malformed
/// input or signature mismatch — the caller then binds nothing.
///
/// The caller supplies the SAME (chain_id, responder_peer, challenger_peer,
/// nonce) it issued the challenge with; a signature made for a different tuple
/// (a relayed or replayed attestation) fails here.
pub fn verify_attestation(
    pubkey: &[u8],
    chain_id: &str,
    responder_peer: &[u8],
    challenger_peer: &[u8],
    nonce: &[u8; 32],
    sig: &[u8],
) -> Option<Address> {
    // Wire-safe: pubkey/sig arrive as slices (serde does not derive fixed arrays
    // above 32). Length-check exactly as block.rs:102-124 before parsing.
    let pk: &[u8; 32] = pubkey.try_into().ok()?;
    let sig: &[u8; 64] = sig.try_into().ok()?;
    let vk = VerifyingKey::from_bytes(pk).ok()?;
    let sig = Signature::from_bytes(sig);
    vk.verify(
        &build_attest_bytes(chain_id, responder_peer, challenger_peer, nonce),
        &sig,
    )
    .ok()?;
    Some(Address::from_public_key(&vk))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wallet::Wallet;

    const CHAIN: &str = "commputer-testnet-3";

    // A responder signs exactly what the primitive builds; the caller reuses the
    // wallet's key bytes and signature bytes on the wire.
    fn attest(w: &Wallet, responder: &[u8], challenger: &[u8], nonce: &[u8; 32]) -> ([u8; 32], [u8; 64]) {
        let sig = w.sign(&build_attest_bytes(CHAIN, responder, challenger, nonce));
        (w.public_key().to_bytes(), sig.to_bytes())
    }

    #[test]
    fn round_trip_binds_the_responder_address() {
        let w = Wallet::generate();
        let (resp, chal, nonce) = (b"peer-R".as_slice(), b"peer-C".as_slice(), &[7u8; 32]);
        let (pk, sig) = attest(&w, resp, chal, nonce);
        let addr = verify_attestation(&pk, CHAIN, resp, chal, nonce, &sig)
            .expect("a genuine attestation must verify");
        assert_eq!(&addr, w.address(), "the bound address must be the signer's");
    }

    #[test]
    fn a_relayed_attestation_is_worthless_to_a_different_peer() {
        // R attests to challenger C. An attacker relays R's proof but presents it
        // as if challenged by C' (or for a different responder peer id). Because
        // both peer ids are in the signed bytes, verification against the
        // attacker's tuple fails — the relay binds nothing.
        let w = Wallet::generate();
        let nonce = &[9u8; 32];
        let (pk, sig) = attest(&w, b"peer-R", b"peer-C", nonce);
        // Same proof, wrong challenger.
        assert!(verify_attestation(&pk, CHAIN, b"peer-R", b"peer-EVIL", nonce, &sig).is_none());
        // Same proof, wrong responder id (attacker claims R's proof is its own).
        assert!(verify_attestation(&pk, CHAIN, b"peer-EVIL", b"peer-C", nonce, &sig).is_none());
    }

    #[test]
    fn a_stale_nonce_or_foreign_chain_fails() {
        let w = Wallet::generate();
        let (pk, sig) = attest(&w, b"peer-R", b"peer-C", &[1u8; 32]);
        assert!(verify_attestation(&pk, CHAIN, b"peer-R", b"peer-C", &[2u8; 32], &sig).is_none());
        assert!(verify_attestation(&pk, "commputer-testnet-4", b"peer-R", b"peer-C", &[1u8; 32], &sig).is_none());
    }

    #[test]
    fn a_wrong_key_for_a_genuine_signature_fails() {
        // A signature from wallet A presented with wallet B's public key: verify
        // fails (the sig was not made by B's key), so no address is bound.
        let (a, b) = (Wallet::generate(), Wallet::generate());
        let nonce = &[3u8; 32];
        let sig = a.sign(&build_attest_bytes(CHAIN, b"peer-R", b"peer-C", nonce)).to_bytes();
        let b_pk = b.public_key().to_bytes();
        assert!(verify_attestation(&b_pk, CHAIN, b"peer-R", b"peer-C", nonce, &sig).is_none());
    }

    #[test]
    fn domain_tag_prevents_cross_protocol_replay() {
        // The attest bytes always start with the domain tag, so they can never
        // equal a block/tx signable message (different prefix).
        let bytes = build_attest_bytes(CHAIN, b"r", b"c", &[0u8; 32]);
        assert!(bytes.starts_with(ATTEST_DOMAIN));
    }
}
