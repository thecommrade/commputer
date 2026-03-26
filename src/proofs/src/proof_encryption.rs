//! Item 158: Proof result encryption.
//!
//! Encrypt proof results in transit so only the verifier can read them.
//! Uses a simple XOR-based stream cipher derived from a shared secret
//! (in production, this would use proper ECDH + AES-GCM).

use sha2::{Digest, Sha256};
use commputer_core::identity::Address;

/// Encrypts and decrypts proof results using a shared secret.
pub struct ProofEncryptor;

/// An encrypted proof result.
#[derive(Debug, Clone)]
pub struct EncryptedProofResult {
    /// The encrypted data.
    pub ciphertext: Vec<u8>,
    /// Nonce used for encryption (ensures unique ciphertexts).
    pub nonce: [u8; 16],
    /// Hash of the plaintext (for integrity verification after decryption).
    pub plaintext_hash: [u8; 32],
    /// The intended recipient (verifier).
    pub recipient: Address,
}

impl ProofEncryptor {
    /// Derive a shared secret between a prover and verifier.
    ///
    /// In production, this would use ECDH key exchange. Here we use
    /// a deterministic derivation from both addresses for simplicity.
    pub fn derive_shared_secret(prover: &Address, verifier: &Address) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(b"proof_shared_secret:");
        // Order-independent: sort addresses so A->B and B->A get the same secret.
        if prover.0 <= verifier.0 {
            hasher.update(prover.0);
            hasher.update(verifier.0);
        } else {
            hasher.update(verifier.0);
            hasher.update(prover.0);
        }
        let result = hasher.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(&result);
        out
    }

    /// Encrypt a proof result for a specific verifier.
    pub fn encrypt(
        plaintext: &[u8],
        prover: &Address,
        verifier: &Address,
        nonce: &[u8; 16],
    ) -> EncryptedProofResult {
        let secret = Self::derive_shared_secret(prover, verifier);
        let keystream = Self::generate_keystream(&secret, nonce, plaintext.len());

        let ciphertext: Vec<u8> = plaintext
            .iter()
            .zip(keystream.iter())
            .map(|(p, k)| p ^ k)
            .collect();

        let plaintext_hash = Self::hash_data(plaintext);

        EncryptedProofResult {
            ciphertext,
            nonce: *nonce,
            plaintext_hash,
            recipient: *verifier,
        }
    }

    /// Decrypt an encrypted proof result.
    pub fn decrypt(
        encrypted: &EncryptedProofResult,
        prover: &Address,
        verifier: &Address,
    ) -> Result<Vec<u8>, String> {
        let secret = Self::derive_shared_secret(prover, verifier);
        let keystream = Self::generate_keystream(&secret, &encrypted.nonce, encrypted.ciphertext.len());

        let plaintext: Vec<u8> = encrypted
            .ciphertext
            .iter()
            .zip(keystream.iter())
            .map(|(c, k)| c ^ k)
            .collect();

        // Verify integrity.
        let hash = Self::hash_data(&plaintext);
        if hash != encrypted.plaintext_hash {
            return Err("Integrity check failed — data may be corrupted or wrong key".into());
        }

        Ok(plaintext)
    }

    /// Generate a deterministic keystream from a secret and nonce.
    fn generate_keystream(secret: &[u8; 32], nonce: &[u8; 16], length: usize) -> Vec<u8> {
        let mut keystream = Vec::with_capacity(length);
        let mut counter: u64 = 0;

        while keystream.len() < length {
            let mut hasher = Sha256::new();
            hasher.update(secret);
            hasher.update(nonce);
            hasher.update(counter.to_le_bytes());
            let block = hasher.finalize();

            let remaining = length - keystream.len();
            keystream.extend_from_slice(&block[..remaining.min(32)]);
            counter += 1;
        }

        keystream
    }

    /// Generate a nonce from a challenge ID and epoch.
    pub fn generate_nonce(challenge_id: &[u8; 32], epoch: u64) -> [u8; 16] {
        let mut hasher = Sha256::new();
        hasher.update(b"proof_nonce:");
        hasher.update(challenge_id);
        hasher.update(epoch.to_le_bytes());
        let result = hasher.finalize();
        let mut nonce = [0u8; 16];
        nonce.copy_from_slice(&result[..16]);
        nonce
    }

    fn hash_data(data: &[u8]) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(data);
        let result = hasher.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(&result);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_addr(n: u8) -> Address {
        let mut a = [0u8; 32];
        a[0] = n;
        Address(a)
    }

    #[test]
    fn item_158_encrypt_decrypt_roundtrip() {
        let prover = test_addr(1);
        let verifier = test_addr(2);
        let nonce = [42u8; 16];
        let plaintext = b"proof result data here";

        let encrypted = ProofEncryptor::encrypt(plaintext, &prover, &verifier, &nonce);
        assert_ne!(encrypted.ciphertext, plaintext);

        let decrypted = ProofEncryptor::decrypt(&encrypted, &prover, &verifier).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn item_158_wrong_verifier_fails() {
        let prover = test_addr(1);
        let verifier = test_addr(2);
        let wrong_verifier = test_addr(3);
        let nonce = [42u8; 16];
        let plaintext = b"secret proof data";

        let encrypted = ProofEncryptor::encrypt(plaintext, &prover, &verifier, &nonce);

        // Wrong key should fail integrity check.
        let result = ProofEncryptor::decrypt(&encrypted, &prover, &wrong_verifier);
        assert!(result.is_err());
    }

    #[test]
    fn item_158_shared_secret_is_symmetric() {
        let a = test_addr(1);
        let b = test_addr(2);

        let secret_ab = ProofEncryptor::derive_shared_secret(&a, &b);
        let secret_ba = ProofEncryptor::derive_shared_secret(&b, &a);
        assert_eq!(secret_ab, secret_ba);
    }

    #[test]
    fn item_158_different_nonces_different_ciphertexts() {
        let prover = test_addr(1);
        let verifier = test_addr(2);
        let plaintext = b"proof result";

        let e1 = ProofEncryptor::encrypt(plaintext, &prover, &verifier, &[1u8; 16]);
        let e2 = ProofEncryptor::encrypt(plaintext, &prover, &verifier, &[2u8; 16]);

        assert_ne!(e1.ciphertext, e2.ciphertext);
    }

    #[test]
    fn item_158_generate_nonce() {
        let nonce = ProofEncryptor::generate_nonce(&[1u8; 32], 42);
        assert_eq!(nonce.len(), 16);
        assert_ne!(nonce, [0u8; 16]);
    }

    #[test]
    fn item_158_tampered_ciphertext_detected() {
        let prover = test_addr(1);
        let verifier = test_addr(2);
        let nonce = [42u8; 16];
        let plaintext = b"important proof data";

        let mut encrypted = ProofEncryptor::encrypt(plaintext, &prover, &verifier, &nonce);
        encrypted.ciphertext[0] ^= 0xFF; // Tamper

        let result = ProofEncryptor::decrypt(&encrypted, &prover, &verifier);
        assert!(result.is_err());
    }
}
