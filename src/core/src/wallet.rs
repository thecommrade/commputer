use ed25519_dalek::{SigningKey, VerifyingKey, Signature, Signer, Verifier};
use rand::RngCore;
use rand::rngs::OsRng;
use crate::identity::Address;

pub struct Wallet {
    signing_key: SigningKey,
    verifying_key: VerifyingKey,
    address: Address,
}

impl Wallet {
    pub fn generate() -> Self {
        let mut secret_bytes = [0u8; 32];
        OsRng.fill_bytes(&mut secret_bytes);
        let signing_key = SigningKey::from_bytes(&secret_bytes);
        let verifying_key = signing_key.verifying_key();
        let address = Address::from_public_key(&verifying_key);
        Self { signing_key, verifying_key, address }
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
}
