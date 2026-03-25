use serde::{Deserialize, Serialize};
use borsh::{BorshDeserialize, BorshSerialize};
use sha2::{Sha256, Digest};
use crate::identity::Address;
use crate::transaction::Transaction;
use crate::proof::EpochProofSummary;

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

/// Block header — the lightweight summary that gets hashed and gossiped.
#[derive(Debug, Clone, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct BlockHeader {
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
    /// Signature of the producer over the header fields.
    pub signature: Vec<u8>,
}

impl BlockHeader {
    /// Compute the hash of this header.
    pub fn hash(&self) -> BlockHash {
        let encoded = borsh::to_vec(self).expect("header serialization should not fail");
        let hash = Sha256::digest(&encoded);
        let mut out = [0u8; 32];
        out.copy_from_slice(&hash);
        BlockHash(out)
    }
}

/// A full block containing header, transactions, and proof summaries.
#[derive(Debug, Clone, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct Block {
    pub header: BlockHeader,
    pub transactions: Vec<Transaction>,
    pub proof_summaries: Vec<EpochProofSummary>,
}

impl Block {
    pub fn hash(&self) -> BlockHash {
        self.header.hash()
    }

    pub fn height(&self) -> u64 {
        self.header.height
    }

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

    let mut next_level = Vec::with_capacity((leaves.len() + 1) / 2);
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
