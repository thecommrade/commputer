use serde::{Deserialize, Serialize};
use borsh::{BorshDeserialize, BorshSerialize};
use sha2::{Sha256, Digest};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use crate::identity::Address;
use crate::transaction::Transaction;
use crate::proof::EpochProofSummary;
use crate::compliance::ComplianceSummary;

fn default_protocol_version() -> u32 { CURRENT_PROTOCOL_VERSION }

/// A 32-byte block hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord,
         Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct BlockHash(pub [u8; 32]);

impl BlockHash {
    pub const GENESIS: Self = Self([0u8; 32]);
}

impl std::fmt::Display for BlockHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", hex::encode(&self.0[..8]))
    }
}

/// Current consensus protocol version. Blocks with a different version are rejected.
pub const CURRENT_PROTOCOL_VERSION: u32 = 1;

/// Block header — the lightweight summary that gets hashed and gossiped.
#[derive(Debug, Clone, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct BlockHeader {
    /// Consensus protocol version (feature 123).
    #[serde(default = "default_protocol_version")]
    pub protocol_version: u32,
    /// Block height (0 = genesis).
    pub height: u64,
    /// Hash of the previous block.
    pub parent_hash: BlockHash,
    /// Merkle root of transactions in this block.
    pub tx_root: [u8; 32],
    /// Merkle root of proof summaries in this block.
    pub proof_root: [u8; 32],
    /// State root after applying this block.
    pub state_root: [u8; 32],
    /// Timestamp (unix seconds).
    pub timestamp: u64,
    /// The validator that produced this block (Snowball anchor).
    pub producer: Address,
    /// Current epoch number.
    pub epoch: u64,
    /// Producer's ed25519 public key (32 bytes). Required for signature verification.
    pub producer_public_key: Vec<u8>,
    /// Signature of the producer over the header fields.
    pub signature: Vec<u8>,
    /// Feature 248: Checkpoint hash — full state root hash at checkpoint intervals (every 1000 blocks).
    #[serde(default)]
    pub checkpoint_hash: Option<[u8; 32]>,
    /// Chain identifier (e.g., "commputer-testnet-1"). Empty string for backwards compat.
    #[serde(default)]
    pub chain_id: String,
}

/// Feature 248: Checkpoint interval for full state root hashing.
pub const CHECKPOINT_HASH_INTERVAL: u64 = 1000;

impl BlockHeader {
    /// Compute the hash of this header.
    pub fn hash(&self) -> BlockHash {
        let encoded = borsh::to_vec(self).expect("header serialization should not fail");
        let hash = Sha256::digest(&encoded);
        let mut out = [0u8; 32];
        out.copy_from_slice(&hash);
        BlockHash(out)
    }

    /// Compute the bytes that the producer signs: all header fields except signature.
    pub fn signable_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        borsh::BorshSerialize::serialize(&self.protocol_version, &mut bytes).unwrap();
        borsh::BorshSerialize::serialize(&self.height, &mut bytes).unwrap();
        borsh::BorshSerialize::serialize(&self.parent_hash, &mut bytes).unwrap();
        borsh::BorshSerialize::serialize(&self.tx_root, &mut bytes).unwrap();
        borsh::BorshSerialize::serialize(&self.proof_root, &mut bytes).unwrap();
        borsh::BorshSerialize::serialize(&self.state_root, &mut bytes).unwrap();
        borsh::BorshSerialize::serialize(&self.timestamp, &mut bytes).unwrap();
        borsh::BorshSerialize::serialize(&self.producer, &mut bytes).unwrap();
        borsh::BorshSerialize::serialize(&self.epoch, &mut bytes).unwrap();
        borsh::BorshSerialize::serialize(&self.chain_id, &mut bytes).unwrap();
        // E5: cover checkpoint_hash so a relay cannot strip/alter it on a signed
        // block. sign_block() and verify_signature() both route through this fn,
        // so producer and verifier stay consistent; without it a stripped
        // checkpoint_hash keeps the signature valid while changing the block hash,
        // manufacturing two validly-signed blocks at one height (equivocation).
        borsh::BorshSerialize::serialize(&self.checkpoint_hash, &mut bytes).unwrap();
        bytes
    }

    /// Verify the producer's signature on this header. Requires the producer's
    /// public key. Returns false if the signature is missing/invalid or the key
    /// doesn't match the producer address.
    pub fn verify_signature(&self, public_key: &[u8]) -> bool {
        if public_key.len() != 32 || self.signature.len() != 64 {
            return false;
        }
        let pk_bytes: &[u8; 32] = match public_key.try_into() {
            Ok(b) => b,
            Err(_) => return false,
        };
        let vk = match VerifyingKey::from_bytes(pk_bytes) {
            Ok(vk) => vk,
            Err(_) => return false,
        };
        // Verify the public key matches the producer address.
        if Address::from_public_key(&vk) != self.producer {
            return false;
        }
        let sig_bytes: &[u8; 64] = match self.signature.as_slice().try_into() {
            Ok(b) => b,
            Err(_) => return false,
        };
        let sig = Signature::from_bytes(sig_bytes);
        vk.verify(&self.signable_bytes(), &sig).is_ok()
    }
}

/// Enforcement flag for producer signatures. Flipped to `true` at the alpha
/// genesis reset (protected batch §2.8): [`Block::verify_producer_signature`]
/// now delegates to the strict check, so every non-genesis block must carry a
/// valid ed25519 signature whose embedded key hashes to the declared producer.
/// Unsigned non-genesis blocks are rejected network-wide. Consensus-affecting —
/// rides the genesis reset (all nodes restart on the flipped binary). A runtime
/// boot assertion in main.rs `run_node` guards against a forgotten flip.
pub const ENFORCE_PRODUCER_SIGNATURES: bool = true;

/// Maximum number of transactions per block.
pub const MAX_TRANSACTIONS_PER_BLOCK: usize = 500;

/// Maximum serialized block size in bytes (1 MB).
pub const MAX_BLOCK_SIZE_BYTES: usize = 1_048_576;

/// A full block containing header, transactions, and proof summaries.
#[derive(Debug, Clone, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct Block {
    pub header: BlockHeader,
    pub transactions: Vec<Transaction>,
    pub proof_summaries: Vec<EpochProofSummary>,
    /// Feature 146: Compliance summary included by block producers.
    #[serde(default)]
    pub compliance_summary: Option<ComplianceSummary>,
    /// Feature 9: Epoch summary included at epoch boundaries.
    #[serde(default)]
    #[borsh(skip)]
    pub epoch_summary: Option<EpochSummary>,
}

impl Block {
    /// Returns the hash of this block (delegated to header).
    pub fn hash(&self) -> BlockHash {
        self.header.hash()
    }

    /// Returns the block height.
    pub fn height(&self) -> u64 {
        self.header.height
    }

    /// Returns true if this is the genesis block (height 0, zero parent hash).
    pub fn is_genesis(&self) -> bool {
        self.header.height == 0 && self.header.parent_hash == BlockHash::GENESIS
    }

    /// Compute the merkle root of this block's transactions.
    pub fn compute_tx_root(&self) -> [u8; 32] {
        merkle_root(
            &self.transactions.iter()
                .map(|tx| tx.hash().0)
                .collect::<Vec<_>>()
        )
    }

    /// Compute the merkle root of this block's proof summaries.
    pub fn compute_proof_root(&self) -> [u8; 32] {
        merkle_root(
            &self.proof_summaries.iter()
                .map(|ps| {
                    let encoded = borsh::to_vec(ps).expect("proof summary serialization");
                    let hash = Sha256::digest(&encoded);
                    let mut out = [0u8; 32];
                    out.copy_from_slice(&hash);
                    out
                })
                .collect::<Vec<_>>()
        )
    }

    /// Verify the producer's signature using the embedded public key.
    ///
    /// Behaviour is governed by [`ENFORCE_PRODUCER_SIGNATURES`]:
    /// - while `false` (pre-signing testnet): unsigned blocks are accepted for
    ///   backward compatibility, signed blocks are still verified;
    /// - once the protected enforcement batch flips it `true` at the alpha
    ///   genesis reset: every non-genesis block must carry a valid signature
    ///   (delegates to [`Block::verify_producer_signature_strict`]).
    pub fn verify_producer_signature(&self) -> bool {
        if ENFORCE_PRODUCER_SIGNATURES {
            return self.verify_producer_signature_strict();
        }
        // Legacy compat: accept unsigned blocks (genesis, pre-signing testnet).
        if self.header.signature.is_empty() && self.header.producer_public_key.is_empty() {
            return true;
        }
        self.header.verify_signature(&self.header.producer_public_key)
    }

    /// Strict producer-signature check for the post-flip network.
    ///
    /// Genesis (height 0) may be unsigned. Every other block MUST carry a
    /// non-empty signature AND public key whose hash equals the producer
    /// address (enforced inside [`BlockHeader::verify_signature`]); an unsigned
    /// non-genesis block is rejected. This closes the forgery hole where an
    /// empty (sig, key) pair let any peer author a block with an arbitrary
    /// producer. Always compiled and unit-tested; wired into the live validate
    /// path only by the founder-gated enforcement batch.
    pub fn verify_producer_signature_strict(&self) -> bool {
        if self.header.height == 0
            && self.header.signature.is_empty()
            && self.header.producer_public_key.is_empty()
        {
            // Genesis is permitted to be unsigned.
            return true;
        }
        // Non-genesis (and any signed genesis) requires a real signature.
        if self.header.signature.is_empty() || self.header.producer_public_key.is_empty() {
            return false;
        }
        self.header.verify_signature(&self.header.producer_public_key)
    }

    /// Check if the block exceeds size limits.
    pub fn within_size_limits(&self) -> bool {
        if self.transactions.len() > MAX_TRANSACTIONS_PER_BLOCK {
            return false;
        }
        let encoded = serde_json::to_vec(self).unwrap_or_default();
        encoded.len() <= MAX_BLOCK_SIZE_BYTES
    }

    /// Verify that the header's tx_root and proof_root match the actual contents.
    pub fn verify_roots(&self) -> bool {
        self.header.tx_root == self.compute_tx_root()
            && self.header.proof_root == self.compute_proof_root()
    }
}

/// Simple merkle root: recursively hash pairs of 32-byte leaves.
/// Empty input returns all zeros. Single leaf returns itself.
fn merkle_root(leaves: &[[u8; 32]]) -> [u8; 32] {
    if leaves.is_empty() {
        return [0u8; 32];
    }
    if leaves.len() == 1 {
        return leaves[0];
    }

    let mut next_level = Vec::with_capacity(leaves.len().div_ceil(2));
    for pair in leaves.chunks(2) {
        let mut hasher = Sha256::new();
        hasher.update(pair[0]);
        if pair.len() == 2 {
            hasher.update(pair[1]);
        } else {
            hasher.update(pair[0]); // duplicate odd leaf
        }
        let hash = hasher.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(&hash);
        next_level.push(out);
    }

    merkle_root(&next_level)
}

/// Feature 7: Compact block announcement — gossip hash + height + producer instead of full block.
/// Peers check if they need the full block and request it via the block request protocol.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockAnnounce {
    /// Hash of the announced block.
    pub hash: BlockHash,
    /// Block height.
    pub height: u64,
    /// Block producer address.
    pub producer: Address,
}

/// Feature 9: Epoch summary included at epoch boundaries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpochSummary {
    /// Epoch number this summary covers.
    pub epoch: u64,
    /// Total emission during this epoch (raw units).
    pub total_emission: u64,
    /// Total burned during this epoch (raw units).
    pub total_burned: u64,
    /// Number of active validators during this epoch.
    pub validator_count: u64,
    /// Sum of composite proof scores across all validators.
    pub proof_scores_total: u64,
    /// Number of compliant validators.
    pub compliant_count: u64,
    /// Number of nerfed validators.
    pub nerfed_count: u64,
}

#[cfg(test)]
mod producer_signature_tests {
    use super::*;
    use crate::wallet::Wallet;
    use crate::signing::sign_block;

    fn unsigned_block(height: u64, producer: Address) -> Block {
        Block {
            header: BlockHeader {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                height,
                parent_hash: BlockHash::GENESIS,
                tx_root: [0u8; 32],
                proof_root: [0u8; 32],
                state_root: [0u8; 32],
                timestamp: 1_700_000_000,
                producer,
                epoch: 0,
                producer_public_key: vec![],
                signature: vec![],
                checkpoint_hash: None,
                chain_id: "test".to_string(),
            },
            transactions: vec![],
            proof_summaries: vec![],
            compliance_summary: None,
            epoch_summary: None,
        }
    }

    #[test]
    fn strict_rejects_unsigned_nongenesis() {
        let w = Wallet::generate();
        let b = unsigned_block(5, *w.address());
        // The forgery hole: empty (sig, key) at height>0 must be rejected.
        assert!(!b.verify_producer_signature_strict());
        // E12: the permissive wrapper now follows the enforcement flag — it
        // rejects the unsigned non-genesis block once ENFORCE_PRODUCER_SIGNATURES
        // is true, and accepts it while the flag is false (INERT). Meaningful in
        // BOTH const states, so it rides the flip in the same commit.
        assert_eq!(b.verify_producer_signature(), !ENFORCE_PRODUCER_SIGNATURES);
    }

    #[test]
    fn strict_accepts_valid_signature() {
        let w = Wallet::generate();
        let mut b = unsigned_block(5, *w.address());
        sign_block(&mut b, &w);
        assert!(b.verify_producer_signature_strict());
        assert!(b.verify_producer_signature());
    }

    #[test]
    fn strict_rejects_key_not_matching_producer() {
        let producer = Wallet::generate();
        let imposter = Wallet::generate();
        // Declared producer is `producer`, but `imposter` signs the block.
        let mut b = unsigned_block(5, *producer.address());
        sign_block(&mut b, &imposter);
        // The signature is valid over the bytes, but the embedded key does not
        // hash to the declared producer address → rejected.
        assert!(!b.verify_producer_signature_strict());
    }

    #[test]
    fn strict_allows_unsigned_genesis() {
        let w = Wallet::generate();
        let g = unsigned_block(0, *w.address());
        assert!(g.verify_producer_signature_strict());
    }
}
