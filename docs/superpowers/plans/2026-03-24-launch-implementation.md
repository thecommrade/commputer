# Commputer v0.1 Launch Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a working L1 blockchain where two nodes can connect, reach consensus, mine $COMME, and grant access to an analytics platform.

**Architecture:** Rust workspace with 7 crates (core, consensus, storage, network, proofs, validator, node) + Tauri desktop app. Core/consensus/storage/proofs are partially built (49 tests passing). Remaining work: wallet/signing, libp2p networking, 4 proof channels, validator lifecycle, main event loop, desktop app.

**Tech Stack:** Rust 2024 edition, libp2p, Tauri, ed25519-dalek, RocksDB (later), tokio async runtime, futures (for StreamExt).

**Spec:** `docs/specs/2026-03-24-launch-scope-design.md`

---

## Phase 1: Wallet & Signing (no dependencies on network)

### Task 1.1: Wallet Key Generation

**Files:**
- Create: `src/core/src/wallet.rs`
- Modify: `src/core/src/lib.rs`
- Modify: `src/core/Cargo.toml`

- [ ] **Step 1: Write the failing test**

```rust
// In src/core/src/wallet.rs
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd src && cargo test -p commputer-core wallet -- --nocapture`
Expected: FAIL — `Wallet` not defined

- [ ] **Step 3: Write minimal implementation**

```rust
use ed25519_dalek::{SigningKey, VerifyingKey, Signature, Signer, Verifier};
use rand::rngs::OsRng;
use crate::identity::Address;

pub struct Wallet {
    signing_key: SigningKey,
    verifying_key: VerifyingKey,
    address: Address,
}

impl Wallet {
    pub fn generate() -> Self {
        let signing_key = SigningKey::generate(&mut OsRng);
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
```

- [ ] **Step 4: Add `pub mod wallet;` to `src/core/src/lib.rs` and `pub use wallet::Wallet;`**

- [ ] **Step 5: Run test to verify it passes**

Run: `cd src && cargo test -p commputer-core wallet -v`
Expected: 3 tests PASS

- [ ] **Step 6: Commit**

```bash
git add src/core/src/wallet.rs src/core/src/lib.rs
git commit -m "feat(core): add wallet key generation and signing"
```

---

### Task 1.2: Seed Phrase (BIP39-style mnemonic)

**Files:**
- Modify: `src/core/src/wallet.rs`
- Modify: `src/core/Cargo.toml`

- [ ] **Step 1: Add `bip39` dependency to core Cargo.toml**

```toml
bip39 = { version = "2", features = ["english"] }
```

- [ ] **Step 2: Write the failing tests**

```rust
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
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cd src && cargo test -p commputer-core wallet -v`
Expected: FAIL — `seed_phrase` method not found

- [ ] **Step 4: Implement seed phrase generation and recovery**

Add to `Wallet`:
```rust
use bip39::{Mnemonic, Language};
use sha2::{Sha256, Digest};

impl Wallet {
    pub fn seed_phrase(&self) -> String {
        // Derive mnemonic from signing key bytes
        let entropy = self.signing_key.to_bytes();
        let mnemonic = Mnemonic::from_entropy(&entropy).expect("32 bytes is valid entropy");
        mnemonic.to_string()
    }

    pub fn from_seed_phrase(phrase: &str) -> Result<Self, crate::error::CommpError> {
        let mnemonic = Mnemonic::parse(phrase)
            .map_err(|e| crate::error::CommpError::Crypto(e.to_string()))?;
        let entropy = mnemonic.to_entropy();
        let signing_key = SigningKey::from_bytes(
            &entropy.try_into()
                .map_err(|_| crate::error::CommpError::Crypto("invalid entropy length".into()))?
        );
        let verifying_key = signing_key.verifying_key();
        let address = Address::from_public_key(&verifying_key);
        Ok(Self { signing_key, verifying_key, address })
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd src && cargo test -p commputer-core wallet -v`
Expected: 6 tests PASS

- [ ] **Step 6: Commit**

```bash
git add src/core/src/wallet.rs src/core/Cargo.toml
git commit -m "feat(core): add BIP39 seed phrase generation and recovery"
```

---

### Task 1.3: Encrypted Keystore

**Files:**
- Create: `src/core/src/keystore.rs`
- Modify: `src/core/src/lib.rs`
- Modify: `src/core/Cargo.toml`

- [ ] **Step 1: Add dependencies**

Add to core Cargo.toml:
```toml
aes-gcm = "0.10"
argon2 = "0.5"
serde_json = { workspace = true }
```

- [ ] **Step 2: Write the failing tests**

```rust
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
```

- [ ] **Step 3: Implement Keystore**

```rust
use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead};
use argon2::Argon2;
use crate::wallet::Wallet;
use crate::error::CommpError;
use std::path::Path;

pub struct Keystore;

impl Keystore {
    pub fn save(wallet: &Wallet, path: &Path, password: &str) -> Result<(), CommpError> {
        let salt: [u8; 16] = rand::random();
        let nonce_bytes: [u8; 12] = rand::random();

        let mut derived_key = [0u8; 32];
        Argon2::default()
            .hash_password_into(password.as_bytes(), &salt, &mut derived_key)
            .map_err(|e| CommpError::Crypto(e.to_string()))?;

        let cipher = Aes256Gcm::new_from_slice(&derived_key)
            .map_err(|e| CommpError::Crypto(e.to_string()))?;
        let nonce = Nonce::from_slice(&nonce_bytes);

        let plaintext = wallet.seed_phrase();
        let ciphertext = cipher.encrypt(nonce, plaintext.as_bytes())
            .map_err(|e| CommpError::Crypto(e.to_string()))?;

        let keystore = serde_json::json!({
            "version": 1,
            "address": format!("{}", wallet.address()),
            "crypto": {
                "salt": hex::encode(salt),
                "nonce": hex::encode(nonce_bytes),
                "ciphertext": hex::encode(ciphertext),
            }
        });

        std::fs::write(path, serde_json::to_string_pretty(&keystore).unwrap())
            .map_err(|e| CommpError::Storage(e.to_string()))?;
        Ok(())
    }

    pub fn load(path: &Path, password: &str) -> Result<Wallet, CommpError> {
        let data = std::fs::read_to_string(path)
            .map_err(|e| CommpError::Storage(e.to_string()))?;
        let keystore: serde_json::Value = serde_json::from_str(&data)
            .map_err(|e| CommpError::Serialization(e.to_string()))?;

        let crypto = &keystore["crypto"];
        let salt = hex::decode(crypto["salt"].as_str().unwrap())
            .map_err(|e| CommpError::Crypto(e.to_string()))?;
        let nonce_bytes = hex::decode(crypto["nonce"].as_str().unwrap())
            .map_err(|e| CommpError::Crypto(e.to_string()))?;
        let ciphertext = hex::decode(crypto["ciphertext"].as_str().unwrap())
            .map_err(|e| CommpError::Crypto(e.to_string()))?;

        let mut derived_key = [0u8; 32];
        Argon2::default()
            .hash_password_into(password.as_bytes(), &salt, &mut derived_key)
            .map_err(|e| CommpError::Crypto(e.to_string()))?;

        let cipher = Aes256Gcm::new_from_slice(&derived_key)
            .map_err(|e| CommpError::Crypto(e.to_string()))?;
        let nonce = Nonce::from_slice(&nonce_bytes);

        let plaintext = cipher.decrypt(nonce, ciphertext.as_ref())
            .map_err(|_| CommpError::Crypto("decryption failed — wrong password?".into()))?;

        let phrase = String::from_utf8(plaintext)
            .map_err(|e| CommpError::Crypto(e.to_string()))?;

        Wallet::from_seed_phrase(&phrase)
    }
}
```

- [ ] **Step 4: Add `pub mod keystore;` to lib.rs**

- [ ] **Step 5: Run tests**

Run: `cd src && cargo test -p commputer-core keystore -v`
Expected: 2 tests PASS

- [ ] **Step 6: Commit**

```bash
git add src/core/src/keystore.rs src/core/src/lib.rs src/core/Cargo.toml
git commit -m "feat(core): add encrypted keystore with Argon2 + AES-256-GCM"
```

---

### Task 1.4: Transaction Signing & Verification

**Files:**
- Create: `src/core/src/signing.rs`
- Modify: `src/core/src/lib.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::wallet::Wallet;
    use crate::transaction::{Transaction, TxKind};
    use crate::token::Amount;
    use crate::identity::Address;

    #[test]
    fn sign_and_verify_transfer() {
        let sender = Wallet::generate();
        let recipient = Address([1u8; 32]);

        let mut tx = Transaction {
            from: *sender.address(),
            nonce: 0,
            kind: TxKind::Transfer {
                to: recipient,
                amount: Amount::from_comme(10),
            },
            signature: vec![],
        };

        sign_transaction(&mut tx, &sender);
        assert!(!tx.signature.is_empty());
        assert!(verify_transaction(&tx, sender.public_key()));
    }

    #[test]
    fn tampered_tx_fails_verification() {
        let sender = Wallet::generate();
        let recipient = Address([1u8; 32]);

        let mut tx = Transaction {
            from: *sender.address(),
            nonce: 0,
            kind: TxKind::Transfer {
                to: recipient,
                amount: Amount::from_comme(10),
            },
            signature: vec![],
        };

        sign_transaction(&mut tx, &sender);

        // Tamper with nonce.
        tx.nonce = 999;
        assert!(!verify_transaction(&tx, sender.public_key()));
    }
}
```

- [ ] **Step 2: Run tests to verify failure**

Run: `cd src && cargo test -p commputer-core signing -v`
Expected: FAIL

- [ ] **Step 3: Implement signing**

```rust
use crate::wallet::Wallet;
use crate::transaction::Transaction;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use borsh::BorshSerialize;

/// Compute the signable bytes of a transaction (everything except the signature field).
fn tx_signable_bytes(tx: &Transaction) -> Vec<u8> {
    let mut bytes = Vec::new();
    tx.from.serialize(&mut bytes).unwrap();
    tx.nonce.serialize(&mut bytes).unwrap();
    tx.kind.serialize(&mut bytes).unwrap();
    bytes
}

pub fn sign_transaction(tx: &mut Transaction, wallet: &Wallet) {
    let bytes = tx_signable_bytes(tx);
    let sig = wallet.sign(&bytes);
    tx.signature = sig.to_bytes().to_vec();
}

pub fn verify_transaction(tx: &Transaction, public_key: &VerifyingKey) -> bool {
    if tx.signature.len() != 64 {
        return false;
    }
    let bytes = tx_signable_bytes(tx);
    let sig_bytes: &[u8; 64] = match tx.signature.as_slice().try_into() {
        Ok(b) => b,
        Err(_) => return false,
    };
    let sig = Signature::from_bytes(sig_bytes);
    public_key.verify(&bytes, &sig).is_ok()
}
```

- [ ] **Step 4: Add `pub mod signing;` to lib.rs**

- [ ] **Step 5: Run tests**

Run: `cd src && cargo test -p commputer-core signing -v`
Expected: 2 tests PASS

- [ ] **Step 6: Run full workspace tests**

Run: `cd src && cargo test --workspace`
Expected: All tests pass (49 existing + 11 new)

- [ ] **Step 7: Commit**

```bash
git add src/core/src/signing.rs src/core/src/lib.rs
git commit -m "feat(core): add transaction signing and verification"
```

---

## Phase 2: libp2p Networking

### Task 2.1: libp2p Node Setup

**Files:**
- Create: `src/network/src/transport.rs`
- Modify: `src/network/src/lib.rs`
- Modify: `src/network/Cargo.toml`

- [ ] **Step 1: Update network Cargo.toml with libp2p**

```toml
[dependencies]
commputer-core = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
borsh = { workspace = true }
tokio = { workspace = true }
tracing = { workspace = true }
thiserror = { workspace = true }
rand = { workspace = true }
hex = "0.4"
libp2p = { workspace = true }
```

- [ ] **Step 2: Write transport.rs — the libp2p swarm setup**

```rust
use libp2p::{
    gossipsub, identify, kad, noise, tcp, yamux,
    Multiaddr, PeerId as Libp2pPeerId, Swarm, SwarmBuilder,
};
use std::time::Duration;

pub struct CommpNetwork {
    pub swarm: Swarm<CommpBehaviour>,
    pub local_peer_id: Libp2pPeerId,
}

#[derive(libp2p::swarm::NetworkBehaviour)]
pub struct CommpBehaviour {
    pub gossipsub: gossipsub::Behaviour,
    pub kademlia: kad::Behaviour<kad::store::MemoryStore>,
    pub identify: identify::Behaviour,
}

impl CommpNetwork {
    pub fn new(listen_port: u16) -> Result<Self, Box<dyn std::error::Error>> {
        let swarm = SwarmBuilder::with_new_identity()
            .with_tokio()
            .with_tcp(
                tcp::Config::default(),
                noise::Config::new,
                yamux::Config::default,
            )?
            .with_behaviour(|key| {
                let peer_id = key.public().to_peer_id();

                let gossipsub_config = gossipsub::ConfigBuilder::default()
                    .heartbeat_interval(Duration::from_secs(1))
                    .build()
                    .expect("valid gossipsub config");
                let gossipsub = gossipsub::Behaviour::new(
                    gossipsub::MessageAuthenticity::Signed(key.clone()),
                    gossipsub_config,
                ).expect("valid gossipsub behaviour");

                let kademlia = kad::Behaviour::new(
                    peer_id,
                    kad::store::MemoryStore::new(peer_id),
                );

                let identify = identify::Behaviour::new(
                    identify::Config::new("/commputer/0.1.0".into(), key.public()),
                );

                CommpBehaviour { gossipsub, kademlia, identify }
            })?
            .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
            .build();

        let local_peer_id = *swarm.local_peer_id();

        let mut network = Self { swarm, local_peer_id };
        let listen_addr: Multiaddr = format!("/ip4/0.0.0.0/tcp/{}", listen_port).parse()?;
        network.swarm.listen_on(listen_addr)?;

        Ok(network)
    }

    pub fn dial(&mut self, addr: Multiaddr) -> Result<(), Box<dyn std::error::Error>> {
        self.swarm.dial(addr)?;
        Ok(())
    }
}
```

- [ ] **Step 3: Add `pub mod transport;` to network lib.rs**

- [ ] **Step 4: Verify it compiles**

Run: `cd src && cargo check -p commputer-network`
Expected: Compiles with warnings at most

- [ ] **Step 5: Commit**

```bash
git add src/network/
git commit -m "feat(network): add libp2p transport with gossipsub + kademlia + identify"
```

---

### Task 2.2: Gossipsub Topics & Message Serialization

**Files:**
- Create: `src/network/src/topics.rs`
- Modify: `src/network/src/lib.rs`

- [ ] **Step 1: Define gossipsub topics**

```rust
use libp2p::gossipsub::IdentTopic;

pub const TOPIC_BLOCKS: &str = "commputer/blocks/0.1";
pub const TOPIC_TRANSACTIONS: &str = "commputer/txs/0.1";
pub const TOPIC_CONSENSUS: &str = "commputer/consensus/0.1";
pub const TOPIC_PROOFS: &str = "commputer/proofs/0.1";

pub fn block_topic() -> IdentTopic { IdentTopic::new(TOPIC_BLOCKS) }
pub fn tx_topic() -> IdentTopic { IdentTopic::new(TOPIC_TRANSACTIONS) }
pub fn consensus_topic() -> IdentTopic { IdentTopic::new(TOPIC_CONSENSUS) }
pub fn proof_topic() -> IdentTopic { IdentTopic::new(TOPIC_PROOFS) }

pub fn all_topics() -> Vec<IdentTopic> {
    vec![block_topic(), tx_topic(), consensus_topic(), proof_topic()]
}
```

- [ ] **Step 2: Subscribe to all topics in CommpNetwork::new()**

Add after building the swarm:
```rust
for topic in topics::all_topics() {
    network.swarm.behaviour_mut().gossipsub.subscribe(&topic)?;
}
```

- [ ] **Step 3: Verify it compiles**

Run: `cd src && cargo check -p commputer-network`

- [ ] **Step 4: Commit**

```bash
git add src/network/
git commit -m "feat(network): add gossipsub topics for blocks, txs, consensus, proofs"
```

---

### Task 2.3: Seed Node Connection

**Files:**
- Modify: `src/network/src/transport.rs`

- [ ] **Step 1: Add seed node configuration**

```rust
pub const SEED_NODES: &[&str] = &[
    // Founder-operated seed nodes. Replace with real addresses at launch.
    // Format: /ip4/<IP>/tcp/<PORT>/p2p/<PEER_ID>
];

impl CommpNetwork {
    pub fn connect_to_seeds(&mut self) -> usize {
        let mut connected = 0;
        for addr_str in SEED_NODES {
            if let Ok(addr) = addr_str.parse::<Multiaddr>() {
                if self.dial(addr).is_ok() {
                    connected += 1;
                }
            }
        }
        connected
    }
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cd src && cargo check -p commputer-network`

- [ ] **Step 3: Commit**

```bash
git add src/network/
git commit -m "feat(network): add seed node connection infrastructure"
```

---

## Phase 3: Remaining Proof Channels

### Task 3.1: GPU Proof Channel (basic)

**Files:**
- Create: `src/proofs/src/gpu.rs`
- Modify: `src/proofs/src/lib.rs`
- Modify: `src/proofs/src/verifier.rs`

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use commputer_core::identity::Address;
    use commputer_core::proof::{ProofChallenge, ResourceChannel};

    fn test_addr() -> Address { Address([1u8; 32]) }

    fn make_gpu_challenge() -> ProofChallenge {
        let mut payload = vec![0x02]; // GPU marker
        payload.extend_from_slice(&[42u8; 32]);
        ProofChallenge {
            channel: ResourceChannel::Gpu,
            challenge_id: [1u8; 32],
            epoch: 0,
            target: test_addr(),
            payload,
            deadline_block: 100,
        }
    }

    #[test]
    fn solve_gpu_challenge() {
        let challenge = make_gpu_challenge();
        let response = GpuProver::solve(&challenge, test_addr());
        assert!(!response.result.is_empty());
    }

    #[test]
    fn verify_gpu_proof() {
        let challenge = make_gpu_challenge();
        let response = GpuProver::solve(&challenge, test_addr());
        assert!(GpuProver::verify(&challenge, &response));
    }
}
```

- [ ] **Step 2: Implement basic GPU prover**

GPU proof at launch: large matrix multiplication with deterministic seed. Verifiable by recomputing. Tests real floating-point throughput without requiring actual GPU libraries.

```rust
use commputer_core::proof::{ProofChallenge, ProofResponse, ResourceChannel};
use commputer_core::identity::Address;
use sha2::{Sha256, Digest};
use std::time::Instant;

pub struct GpuProver;

impl GpuProver {
    pub fn solve(challenge: &ProofChallenge, validator: Address) -> ProofResponse {
        let start = Instant::now();
        let seed = &challenge.payload[1..]; // Skip type marker

        // Matrix multiply benchmark: generate two 64x64 matrices from seed, multiply them.
        let matrix_a = Self::generate_matrix(seed, 0);
        let matrix_b = Self::generate_matrix(seed, 1);
        let result_matrix = Self::matrix_multiply(&matrix_a, &matrix_b);

        // Hash the result matrix as the proof.
        let result_hash = Sha256::digest(&Self::matrix_to_bytes(&result_matrix));

        ProofResponse {
            challenge_id: challenge.challenge_id,
            validator,
            result: result_hash.to_vec(),
            compute_time_ms: start.elapsed().as_millis() as u64,
            signature: vec![],
        }
    }

    pub fn verify(challenge: &ProofChallenge, response: &ProofResponse) -> bool {
        let seed = &challenge.payload[1..];
        let matrix_a = Self::generate_matrix(seed, 0);
        let matrix_b = Self::generate_matrix(seed, 1);
        let result_matrix = Self::matrix_multiply(&matrix_a, &matrix_b);
        let expected = Sha256::digest(&Self::matrix_to_bytes(&result_matrix));
        expected[..] == response.result[..]
    }

    fn generate_matrix(seed: &[u8], offset: u8) -> Vec<Vec<f64>> {
        let mut hasher = Sha256::new();
        hasher.update(seed);
        hasher.update([offset]);
        let hash = hasher.finalize();

        let size = 64;
        let mut matrix = vec![vec![0.0f64; size]; size];
        for i in 0..size {
            for j in 0..size {
                let idx = (i * size + j) % 32;
                matrix[i][j] = hash[idx] as f64 / 255.0;
            }
        }
        matrix
    }

    fn matrix_multiply(a: &[Vec<f64>], b: &[Vec<f64>]) -> Vec<Vec<f64>> {
        let n = a.len();
        let mut result = vec![vec![0.0f64; n]; n];
        for i in 0..n {
            for k in 0..n {
                let a_ik = a[i][k];
                for j in 0..n {
                    result[i][j] += a_ik * b[k][j];
                }
            }
        }
        result
    }

    fn matrix_to_bytes(matrix: &[Vec<f64>]) -> Vec<u8> {
        matrix.iter().flat_map(|row| {
            row.iter().flat_map(|v| v.to_le_bytes())
        }).collect()
    }
}
```

- [ ] **Step 3: Add `pub mod gpu;` to proofs lib.rs, update verifier**

- [ ] **Step 4: Run tests**

Run: `cd src && cargo test -p commputer-proofs gpu -v`
Expected: 2 tests PASS

- [ ] **Step 5: Commit**

```bash
git add src/proofs/
git commit -m "feat(proofs): add basic GPU proof channel (matrix multiply benchmark)"
```

---

### Task 3.2: Storage Proof Channel (basic)

**Files:**
- Create: `src/proofs/src/storage_proof.rs`
- Modify: `src/proofs/src/lib.rs`

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn solve_storage_challenge() {
    let data = StorageProver::generate_test_data(&[42u8; 32], 1024); // 1KB
    let challenge = StorageProver::make_challenge(&[42u8; 32], 0, Address([1u8; 32]), 100);
    let response = StorageProver::solve(&challenge, &data, Address([1u8; 32]));
    assert!(StorageProver::verify(&challenge, &response, &data));
}

#[test]
fn wrong_data_fails() {
    let data = StorageProver::generate_test_data(&[42u8; 32], 1024);
    let wrong_data = StorageProver::generate_test_data(&[99u8; 32], 1024);
    let challenge = StorageProver::make_challenge(&[42u8; 32], 0, Address([1u8; 32]), 100);
    let response = StorageProver::solve(&challenge, &wrong_data, Address([1u8; 32]));
    assert!(!StorageProver::verify(&challenge, &response, &data));
}
```

- [ ] **Step 2: Implement StorageProver** — Proof of Retrievability: challenge asks for hash of random chunks at specific offsets. Validator must have the actual data to respond correctly.

- [ ] **Step 3: Run tests, verify pass**

- [ ] **Step 4: Commit**

```bash
git commit -m "feat(proofs): add basic storage proof channel (chunk retrievability)"
```

---

### Task 3.3: RAM Proof Channel (basic)

**Files:**
- Create: `src/proofs/src/ram.rs`
- Modify: `src/proofs/src/lib.rs`

- [ ] **Step 1: Write failing tests**

- [ ] **Step 2: Implement RamProver** — Memory-hard challenge: allocate a large buffer, fill it with deterministic data, perform random reads that require the buffer to actually exist in memory (not swapped). Hash the read results.

- [ ] **Step 3: Run tests, verify pass**

- [ ] **Step 4: Commit**

```bash
git commit -m "feat(proofs): add basic RAM proof channel (memory-hard challenge)"
```

---

### Task 3.4: Bandwidth Proof Channel (basic)

**Files:**
- Create: `src/proofs/src/bandwidth.rs`
- Modify: `src/proofs/src/lib.rs`

- [ ] **Step 1: Write failing tests**

- [ ] **Step 2: Implement BandwidthProver** — At launch, this is a self-report with timing verification. The node generates a data payload, times how long it takes to hash it at network-transfer speeds, and reports. Full peer-to-peer transfer measurement comes later.

- [ ] **Step 3: Run tests, verify pass**

- [ ] **Step 4: Commit**

```bash
git commit -m "feat(proofs): add basic bandwidth proof channel (timed transfer)"
```

---

## Phase 4: Validator Lifecycle

### Task 4.1: Validator State Machine

**Files:**
- Modify: `src/validator/src/lib.rs`
- Create: `src/validator/src/lifecycle.rs`
- Modify: `src/validator/Cargo.toml`

- [ ] **Step 1: Write failing tests for validator states**

```rust
#[test]
fn new_validator_starts_idle() {
    let v = ValidatorState::new();
    assert_eq!(v.status(), ValidatorStatus::Idle);
}

#[test]
fn register_transitions_to_active() {
    let mut v = ValidatorState::new();
    v.register(50); // 50% contribution
    assert_eq!(v.status(), ValidatorStatus::Active);
    assert_eq!(v.contribution_percent(), 50);
}

#[test]
fn update_contribution() {
    let mut v = ValidatorState::new();
    v.register(50);
    v.update_contribution(80);
    assert_eq!(v.contribution_percent(), 80);
}

#[test]
fn deregister_transitions_to_idle() {
    let mut v = ValidatorState::new();
    v.register(50);
    v.deregister();
    assert_eq!(v.status(), ValidatorStatus::Idle);
}
```

- [ ] **Step 2: Implement ValidatorState** — state machine: Idle → Active → (Contributing/Nerfed) → Idle

- [ ] **Step 3: Run tests, verify pass**

- [ ] **Step 4: Commit**

```bash
git commit -m "feat(validator): add validator state machine and lifecycle"
```

---

## Phase 5: Anti-Scale Enforcement & Compliance

### Task 5.1: Basic Same-IP Compliance Detection

**Files:**
- Create: `src/validator/src/compliance_check.rs`
- Modify: `src/validator/src/lib.rs`
- Modify: `src/validator/Cargo.toml`

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn single_node_per_ip_is_compliant() {
    let mut checker = ComplianceChecker::new();
    checker.register_node(addr(1), "192.168.1.10".into());
    assert_eq!(checker.check(&addr(1)), ComplianceStatus::Compliant);
}

#[test]
fn two_nodes_same_ip_flagged() {
    let mut checker = ComplianceChecker::new();
    checker.register_node(addr(1), "192.168.1.10".into());
    checker.register_node(addr(2), "192.168.1.10".into());
    assert_eq!(checker.check(&addr(1)), ComplianceStatus::NerfedIncidental);
    assert_eq!(checker.check(&addr(2)), ComplianceStatus::NerfedIncidental);
}

#[test]
fn same_subnet_flagged() {
    let mut checker = ComplianceChecker::new();
    checker.register_node(addr(1), "192.168.1.10".into());
    checker.register_node(addr(2), "192.168.1.11".into());
    // Same /24 subnet — flagged as incidental
    assert_eq!(checker.check(&addr(2)), ComplianceStatus::NerfedIncidental);
}

#[test]
fn compliance_restored_on_deregister() {
    let mut checker = ComplianceChecker::new();
    checker.register_node(addr(1), "192.168.1.10".into());
    checker.register_node(addr(2), "192.168.1.10".into());
    checker.deregister_node(&addr(2));
    assert_eq!(checker.check(&addr(1)), ComplianceStatus::Compliant);
}
```

- [ ] **Step 2: Implement ComplianceChecker** — tracks IP → validator mapping, flags same-IP and same-/24-subnet nodes

- [ ] **Step 3: Run tests, verify pass**

- [ ] **Step 4: Commit**

```bash
git commit -m "feat(validator): add basic IP-range compliance detection"
```

---

### Task 5.2: Nerf Rate Application in Emission

**Files:**
- Modify: `src/consensus/src/emission.rs`
- Modify: `src/storage/src/state.rs`

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn nerfed_validator_earns_20_percent() {
    let full_rate = schedule.per_validator_daily_rate(1000);
    let nerf = NerfRate::INITIAL; // 80% nerf
    let nerfed_rate = (full_rate as f64 * nerf.reward_multiplier()) as u64;
    assert_eq!(nerfed_rate, full_rate / 5); // 20% of full
}
```

- [ ] **Step 2: Wire nerf rate into epoch emission distribution in ChainState**

- [ ] **Step 3: Run tests, verify pass**

- [ ] **Step 4: Commit**

```bash
git commit -m "feat(consensus): apply nerf rate to emission distribution"
```

---

## Phase 6: Main Event Loop

### Task 6.1: Event Loop Structure & Swarm Handling

**Files:**
- Create: `src/node/src/event_loop.rs`
- Modify: `src/node/Cargo.toml`

Add `futures = "0.3"` to workspace Cargo.toml and node Cargo.toml.

- [ ] **Step 1: Define EventLoop struct and NodeEvent enum**

- [ ] **Step 2: Implement `handle_swarm_event` — route gossipsub messages to block/tx/consensus/proof handlers**

- [ ] **Step 3: Verify it compiles**

- [ ] **Step 4: Commit**

```bash
git commit -m "feat(node): add event loop structure and swarm event routing"
```

---

### Task 6.2: Epoch Tick — Emission & Reward Distribution

**Files:**
- Modify: `src/node/src/event_loop.rs`

- [ ] **Step 1: Implement `handle_epoch_tick`**

Wire `ChannelAllocation::from_demand()` from consensus/emission.rs into the epoch handler. At each epoch boundary:
1. Collect proof summaries from all validators this epoch
2. Calculate demand per channel
3. Compute channel allocation using `ChannelAllocation::from_demand()`
4. Distribute rewards to validators (applying nerf rate for non-compliant)
5. Record emission in chain state

- [ ] **Step 2: Verify it compiles**

- [ ] **Step 3: Commit**

```bash
git commit -m "feat(node): wire demand-weighted emission into epoch handler"
```

---

### Task 6.3: Block Production & Consensus

**Files:**
- Modify: `src/node/src/event_loop.rs`

- [ ] **Step 1: Implement `handle_block_tick`**

1. Use `AnchorSelector::select()` to check if this node is the anchor
2. If anchor: collect pending transactions, create block, broadcast via gossipsub
3. If not anchor: participate in Snowball voting for received blocks

- [ ] **Step 2: Verify it compiles**

- [ ] **Step 3: Commit**

```bash
git commit -m "feat(node): add block production and Snowball consensus participation"
```

---

### Task 6.4: Wire Event Loop into main.rs

**Files:**
- Modify: `src/node/src/main.rs`

- [ ] **Step 1: Replace "waiting for peers" with actual event loop startup**

- [ ] **Step 2: Verify node starts and runs**

Run: `cd src && cargo run -p commputer -- --testnet`
Expected: Node starts, listens, prints periodic epoch and block status

- [ ] **Step 3: Commit**

```bash
git commit -m "feat(node): wire event loop into main binary"
```

---

### Task 6.5: Burst Compute User Flow

**Files:**
- Modify: `src/node/src/event_loop.rs`
- Modify: `src/storage/src/state.rs`

- [ ] **Step 1: Write test for burst compute transaction processing**

```rust
#[test]
fn burst_compute_burns_coins() {
    // Create account with 10 COMME, submit BurstCompute tx for 2 COMME
    // Verify: balance = 8, total_burned increased by 2
}
```

- [ ] **Step 2: Ensure BurstCompute transactions are accepted and broadcast via gossipsub**

- [ ] **Step 3: Run tests, verify pass**

- [ ] **Step 4: Commit**

```bash
git commit -m "feat(node): add burst compute transaction flow with burn"
```

---

### Task 6.6: Grace Period Logic

**Files:**
- Modify: `src/storage/src/account.rs`

- [ ] **Step 1: Write tests for grace period tracking in the epoch handler**

```rust
#[test]
fn online_validator_accumulates_grace() {
    // Validator online for 1 epoch (3600s) → grace increases by 3600
}

#[test]
fn offline_validator_drains_grace_1_to_1() {
    // Validator offline for 1000s → grace decreases by 1000
}

#[test]
fn refill_rate_is_2_to_1() {
    // Drain 10 days, come back online 5 days → fully restored
}
```

- [ ] **Step 2: Wire grace tracking into epoch tick handler**

- [ ] **Step 3: Run tests, verify pass**

- [ ] **Step 4: Commit**

```bash
git commit -m "feat(storage): wire grace period drain/refill into epoch handler"
```

---

### Task 6.7: Two-Node Integration Test

**Files:**
- Create: `src/tests/integration_test.rs` (workspace-level integration test)

This is the critical milestone: two nodes connecting and reaching consensus.

- [ ] **Step 1: Write integration test that starts two nodes on different ports**

- [ ] **Step 2: Verify they discover each other via direct dial**

- [ ] **Step 3: Send a transaction from node A, verify node B receives it via gossip**

- [ ] **Step 4: Verify both nodes agree on block contents after a few rounds**

- [ ] **Step 5: Commit**

```bash
git commit -m "test: add two-node integration test for gossip and consensus"
```

---

### Task 5.2: Two-Node Integration Test

**Files:**
- Create: `tests/integration/two_node_test.rs`

This is the critical milestone: two nodes connecting and reaching consensus.

- [ ] **Step 1: Write integration test that starts two nodes on different ports**

- [ ] **Step 2: Verify they discover each other via direct dial (no seed nodes needed for test)**

- [ ] **Step 3: Send a transaction from node A, verify node B receives it via gossip**

- [ ] **Step 4: Verify both nodes agree on block contents after a few rounds**

- [ ] **Step 5: Commit**

```bash
git commit -m "test: add two-node integration test for gossip and consensus"
```

---

## Phase 7: Desktop App (Tauri)

### Task 7.1: Tauri App Scaffold

**Files:**
- Create: `app/` directory (Tauri project)
- Create: `app/src-tauri/` (Rust backend)
- Create: `app/src/` (Web frontend)

- [ ] **Step 1: Install Tauri CLI**

Run: `cargo install create-tauri-app`

- [ ] **Step 2: Scaffold the Tauri app**

Run: `cd /home/operator/Coin && cargo create-tauri-app app --template vanilla-ts`

- [ ] **Step 3: Add commputer crates as dependencies to `app/src-tauri/Cargo.toml`**

- [ ] **Step 4: Verify the app builds and opens a window**

Run: `cd app && cargo tauri dev`

- [ ] **Step 5: Commit**

```bash
git commit -m "feat(app): scaffold Tauri desktop application"
```

---

### Task 7.2: Core UI — Resource Slider + Wallet

**Files:**
- Modify: `app/src/index.html`
- Modify: `app/src/main.ts`
- Create: `app/src-tauri/src/commands.rs`

- [ ] **Step 1: Create Tauri commands** — `get_wallet_info`, `set_contribution`, `get_chain_status`

- [ ] **Step 2: Build the UI** — resource slider (2-100%), wallet address display, $COMME balance, "Analytics" button (grayed out until 1+ COMME)

- [ ] **Step 3: Wire UI to Tauri commands**

- [ ] **Step 4: Verify the app displays wallet info and the slider works**

- [ ] **Step 5: Commit**

```bash
git commit -m "feat(app): add resource slider, wallet display, and analytics button"
```

---

### Task 7.3: Analytics Platform Auth + Earn It Path

**Files:**
- Create: `app/src-tauri/src/analytics.rs`

- [ ] **Step 1: Implement access check logic**

Two paths to analytics access:
- **Hold path:** Wallet balance >= 1 COMME → access granted
- **Earn It path:** Validator is active at 100% contribution of reference node → access granted while running

```rust
fn has_analytics_access(account: &Account, validator_state: &ValidatorState) -> bool {
    // Hold path
    if account.balance.whole_comme() >= 1 {
        return true;
    }
    // Earn It path — contributing full desktop
    if validator_state.status() == ValidatorStatus::Active
        && validator_state.contribution_percent() == 100 {
        return true;
    }
    false
}
```

- [ ] **Step 2: Implement signed request to analytics platform**

The app signs a timestamp + wallet address with the private key. The analytics platform verifies the signature against the chain to confirm access (balance or contribution).

- [ ] **Step 3: Wire "Analytics" button — enabled when `has_analytics_access` returns true, opens platform in browser with auth token**

- [ ] **Step 4: Add burst compute button** — user can spend $COMME for extra compute. Shows current balance, input amount, confirmation, submits BurstCompute transaction.

- [ ] **Step 5: Test the flow end-to-end**

- [ ] **Step 6: Commit**

```bash
git commit -m "feat(app): add analytics auth with Hold + Earn It paths, burst compute UI"
```

---

## Phase Summary

| Phase | Tasks | What It Produces |
|-------|-------|-----------------|
| 1. Wallet & Signing | 4 tasks | Key generation, seed phrases, encrypted storage, tx signing |
| 2. libp2p Networking | 3 tasks | Real P2P connections, gossipsub, seed nodes |
| 3. Proof Channels | 4 tasks | All 5 channels operational (CPU prod, 4 basic) |
| 4. Validator Lifecycle | 1 task | Validator state machine |
| 5. Anti-Scale Enforcement | 2 tasks | Same-IP compliance detection, nerf rate in emission |
| 6. Event Loop | 7 tasks | Swarm handling, epoch emission, block production, burst burns, grace periods, two-node test |
| 7. Desktop App | 3 tasks | Tauri app with slider, wallet, analytics auth (Hold + Earn It), burst compute UI |

**Total: 24 tasks, ~7 phases**

Each phase produces working, testable software. Phases 1-5 can be parallelized. Phase 6 depends on 1-5. Phase 7 depends on 6.
