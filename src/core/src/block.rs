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
}
