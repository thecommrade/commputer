use ed25519_dalek::{SigningKey, VerifyingKey, Signature, Signer, Verifier};
use rand::RngCore;
use rand::rngs::OsRng;
use bip39::Mnemonic;
use zeroize::{Zeroize, Zeroizing};
use crate::identity::Address;
use crate::error::CommpError;

/// A Commputer wallet backed by an ed25519 signing key.
/// Supports BIP39 seed phrase generation and recovery.
pub struct Wallet {
    signing_key: SigningKey,
    verifying_key: VerifyingKey,
    address: Address,
}

impl Wallet {
    /// Generate a new wallet with a random ed25519 keypair.
    pub fn generate() -> Self {
        let mut secret_bytes = [0u8; 32];
        OsRng.fill_bytes(&mut secret_bytes);
        Self::from_secret_bytes(secret_bytes)
    }

    fn from_secret_bytes(secret_bytes: [u8; 32]) -> Self {
        let signing_key = SigningKey::from_bytes(&secret_bytes);
        let verifying_key = signing_key.verifying_key();
        let address = Address::from_public_key(&verifying_key);
        Self { signing_key, verifying_key, address }
    }

    /// Returns the 24-word BIP39 seed phrase for this wallet.
    ///
    /// The 32-byte signing key is used directly as BIP39 entropy
    /// (256 bits → 24 words).
    pub fn seed_phrase(&self) -> String {
        // Wrap the raw private-key entropy in `Zeroizing` so the transient
        // 32-byte copy is scrubbed from the stack when this function returns,
        // rather than lingering in freed memory (matches the Drop-impl intent).
        let entropy = Zeroizing::new(self.signing_key.to_bytes());
        // 32 bytes of entropy always produces a valid 24-word mnemonic.
        //
        // NOTE: the intermediate `Mnemonic` also encodes this entropy. The
        // bip39 crate's optional `zeroize` feature is not enabled in this
        // workspace, so its internal buffer cannot be scrubbed from here; we
        // keep the `Mnemonic` unnamed so its temporary is dropped as early as
        // possible (immediately after `to_string()`).
        Mnemonic::from_entropy(&entropy[..])
            .expect("32-byte entropy is always valid for BIP39")
            .to_string()
    }

    /// Recovers a wallet from a 24-word BIP39 seed phrase.
    ///
    /// The mnemonic entropy (32 bytes) is used directly as the signing key.
    pub fn from_seed_phrase(phrase: &str) -> Result<Self, CommpError> {
        let mnemonic = Mnemonic::parse(phrase)
            .map_err(|e| CommpError::Crypto(format!("invalid seed phrase: {e}")))?;
        let (entropy_arr, entropy_len) = mnemonic.to_entropy_array();
        if entropy_len != 32 {
            return Err(CommpError::Crypto(
                format!("expected 32 bytes of entropy (24 words), got {entropy_len}"),
            ));
        }
        let mut secret_bytes = [0u8; 32];
        secret_bytes.copy_from_slice(&entropy_arr[..32]);
        Ok(Self::from_secret_bytes(secret_bytes))
    }

    /// Returns this wallet's on-chain address.
    pub fn address(&self) -> &Address { &self.address }
    /// Returns the public verifying key.
    pub fn public_key(&self) -> &VerifyingKey { &self.verifying_key }

    /// Sign arbitrary bytes with this wallet's private key.
    pub fn sign(&self, message: &[u8]) -> Signature {
        self.signing_key.sign(message)
    }

    /// Verify a signature against this wallet's public key.
    pub fn verify(&self, message: &[u8], signature: &Signature) -> bool {
        self.verifying_key.verify(message, signature).is_ok()
    }
}

impl Drop for Wallet {
    fn drop(&mut self) {
        // Zeroize the signing key bytes to prevent key material from lingering in memory.
        let mut key_bytes = self.signing_key.to_bytes();
        key_bytes.zeroize();
        // Overwrite the signing key with zeroed bytes.
        self.signing_key = SigningKey::from_bytes(&key_bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_new_wallet() {
        let wallet = Wallet::generate();
        assert_eq!(wallet.address().0.len(), 32);
    }

    #[test]
    fn sign_and_verify() {
        let wallet = Wallet::generate();
        let msg = b"hello commputer";
        let sig = wallet.sign(msg);
        assert!(wallet.verify(msg, &sig));
    }

    #[test]
    fn wrong_message_fails_verify() {
        let wallet = Wallet::generate();
        let sig = wallet.sign(b"hello");
        assert!(!wallet.verify(b"wrong", &sig));
    }

    #[test]
    fn seed_phrase_generates_24_words() {
        let wallet = Wallet::generate();
        let phrase = wallet.seed_phrase();
        assert_eq!(phrase.split_whitespace().count(), 24);
    }

    #[test]
    fn recover_wallet_from_seed_phrase() {
        let wallet = Wallet::generate();
        let phrase = wallet.seed_phrase();
        let recovered = Wallet::from_seed_phrase(&phrase).unwrap();
        assert_eq!(wallet.address(), recovered.address());
    }

    #[test]
    fn invalid_seed_phrase_returns_error() {
        assert!(Wallet::from_seed_phrase("not a valid seed phrase").is_err());
    }

    #[test]
    fn seed_phrase_encodes_exact_signing_key_entropy() {
        // Guards the `Zeroizing`-wrapped entropy slice conversion in
        // seed_phrase(): the phrase must encode the full 32-byte signing key
        // verbatim, so recovering from it must reproduce identical key bytes.
        let wallet = Wallet::generate();
        let original_key_bytes = wallet.signing_key.to_bytes();
        let phrase = wallet.seed_phrase();
        let recovered = Wallet::from_seed_phrase(&phrase).unwrap();
        assert_eq!(
            original_key_bytes,
            recovered.signing_key.to_bytes(),
            "seed phrase must round-trip the exact 32-byte signing key"
        );
    }

    #[test]
    fn seed_phrase_is_deterministic() {
        // Calling seed_phrase() repeatedly must yield the same phrase; the
        // per-call `Zeroizing` copy must not perturb the produced mnemonic.
        let wallet = Wallet::generate();
        assert_eq!(wallet.seed_phrase(), wallet.seed_phrase());
    }

    #[test]
    fn wallet_drop_does_not_panic() {
        // Verify that creating and dropping a wallet doesn't panic
        // (exercises the Drop impl with zeroization).
        let wallet = Wallet::generate();
        let _addr = *wallet.address();
        drop(wallet);
    }
}
