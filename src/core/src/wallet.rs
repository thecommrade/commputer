use ed25519_dalek::{SigningKey, VerifyingKey, Signature, Signer, Verifier};
use rand::RngCore;
use rand::rngs::OsRng;
use bip39::Mnemonic;
use crate::identity::Address;
use crate::error::CommpError;

pub struct Wallet {
    signing_key: SigningKey,
    verifying_key: VerifyingKey,
    address: Address,
}

impl Wallet {
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
        let entropy = self.signing_key.to_bytes();
        // 32 bytes of entropy always produces a valid 24-word mnemonic.
        Mnemonic::from_entropy(&entropy)
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

    pub fn address(&self) -> &Address { &self.address }
    pub fn public_key(&self) -> &VerifyingKey { &self.verifying_key }

    pub fn sign(&self, message: &[u8]) -> Signature {
        self.signing_key.sign(message)
    }

    pub fn verify(&self, message: &[u8], signature: &Signature) -> bool {
        self.verifying_key.verify(message, signature).is_ok()
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
}
