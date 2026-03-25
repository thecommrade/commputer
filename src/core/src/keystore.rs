use std::path::Path;

use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead};
use argon2::Argon2;
use rand::RngCore;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};

use crate::error::CommpError;
use crate::wallet::Wallet;

/// Encrypted on-disk keystore.
///
/// The seed phrase is encrypted with AES-256-GCM. The encryption key is
/// derived from the user password using Argon2id with a random 16-byte salt.
/// All binary fields are stored as lowercase hex strings inside a JSON file.
pub struct Keystore;

#[derive(Serialize, Deserialize)]
struct KeystoreFile {
    /// Wallet address (hex), stored for display only — not used during load.
    address: String,
    /// 16-byte Argon2 salt, hex-encoded.
    salt: String,
    /// 12-byte AES-GCM nonce, hex-encoded.
    nonce: String,
    /// AES-256-GCM ciphertext (includes the 16-byte authentication tag appended
    /// by the `aes-gcm` crate), hex-encoded.
    ciphertext: String,
}

impl Keystore {
    /// Encrypt `wallet`'s seed phrase with `password` and write to `path`.
    pub fn save(wallet: &Wallet, path: &Path, password: &str) -> Result<(), CommpError> {
        // Random salt and nonce.
        let mut salt = [0u8; 16];
        let mut nonce_bytes = [0u8; 12];
        OsRng.fill_bytes(&mut salt);
        OsRng.fill_bytes(&mut nonce_bytes);

        // Derive a 32-byte encryption key with Argon2.
        let mut key_bytes = [0u8; 32];
        Argon2::default()
            .hash_password_into(password.as_bytes(), &salt, &mut key_bytes)
            .map_err(|e| CommpError::Crypto(format!("Argon2 key derivation failed: {e}")))?;

        // Encrypt the seed phrase.
        let cipher = Aes256Gcm::new_from_slice(&key_bytes)
            .map_err(|e| CommpError::Crypto(format!("AES key init failed: {e}")))?;
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = cipher
            .encrypt(nonce, wallet.seed_phrase().as_bytes())
            .map_err(|e| CommpError::Crypto(format!("encryption failed: {e}")))?;

        // Serialise.
        let file = KeystoreFile {
            address: hex::encode(wallet.address().0),
            salt: hex::encode(salt),
            nonce: hex::encode(nonce_bytes),
            ciphertext: hex::encode(&ciphertext),
        };
        let json = serde_json::to_string_pretty(&file)
            .map_err(|e| CommpError::Serialization(e.to_string()))?;

        std::fs::write(path, json)
            .map_err(|e| CommpError::Storage(format!("failed to write keystore: {e}")))?;

        Ok(())
    }

    /// Read and decrypt a keystore file, returning the recovered `Wallet`.
    pub fn load(path: &Path, password: &str) -> Result<Wallet, CommpError> {
        let json = std::fs::read_to_string(path)
            .map_err(|e| CommpError::Storage(format!("failed to read keystore: {e}")))?;

        let file: KeystoreFile = serde_json::from_str(&json)
            .map_err(|e| CommpError::Serialization(e.to_string()))?;

        let salt = hex::decode(&file.salt)
            .map_err(|e| CommpError::Crypto(format!("invalid salt hex: {e}")))?;
        let nonce_bytes = hex::decode(&file.nonce)
            .map_err(|e| CommpError::Crypto(format!("invalid nonce hex: {e}")))?;
        let ciphertext = hex::decode(&file.ciphertext)
            .map_err(|e| CommpError::Crypto(format!("invalid ciphertext hex: {e}")))?;

        if nonce_bytes.len() != 12 {
            return Err(CommpError::Crypto(format!(
                "nonce must be 12 bytes, got {}",
                nonce_bytes.len()
            )));
        }

        // Re-derive the key.
        let mut key_bytes = [0u8; 32];
        Argon2::default()
            .hash_password_into(password.as_bytes(), &salt, &mut key_bytes)
            .map_err(|e| CommpError::Crypto(format!("Argon2 key derivation failed: {e}")))?;

        // Decrypt.
        let cipher = Aes256Gcm::new_from_slice(&key_bytes)
            .map_err(|e| CommpError::Crypto(format!("AES key init failed: {e}")))?;
        let nonce = Nonce::from_slice(&nonce_bytes);
        let plaintext = cipher
            .decrypt(nonce, ciphertext.as_ref())
            .map_err(|_| CommpError::Crypto("decryption failed — wrong password?".into()))?;

        let seed_phrase = String::from_utf8(plaintext)
            .map_err(|e| CommpError::Crypto(format!("seed phrase is not valid UTF-8: {e}")))?;

        Wallet::from_seed_phrase(&seed_phrase)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wallet::Wallet;
    use std::path::PathBuf;

    #[test]
    fn save_and_load_keystore() {
        let wallet = Wallet::generate();
        let path = PathBuf::from("/tmp/commputer-test-keystore.json");
        let password = "test-password-123";

        Keystore::save(&wallet, &path, password).unwrap();
        let loaded = Keystore::load(&path, password).unwrap();

        assert_eq!(wallet.address(), loaded.address());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn wrong_password_fails() {
        let wallet = Wallet::generate();
        let path = PathBuf::from("/tmp/commputer-test-keystore-bad.json");

        Keystore::save(&wallet, &path, "correct").unwrap();
        let result = Keystore::load(&path, "wrong");

        assert!(result.is_err());
        std::fs::remove_file(&path).ok();
    }
}
