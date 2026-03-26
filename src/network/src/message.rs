use serde::{Deserialize, Serialize};
use commputer_core::block::{Block, BlockHash, BlockHeader};
use commputer_core::transaction::{Transaction, TxHash};
use commputer_core::proof::{ProofChallenge, ProofResponse};
use crate::peer::PeerId;

/// All message types that flow over the Commputer P2P network.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkMessage {
    /// Who sent this message.
    pub sender: PeerId,
    /// Message nonce for deduplication.
    pub nonce: u64,
    /// The actual content.
    pub kind: MessageKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MessageKind {
    // --- Gossip layer (fast, broadcast) ---

    /// New block announcement.
    NewBlock(Block),
    /// New transaction to be included in a block.
    NewTransaction(Transaction),
    /// Snowball query: "which block do you prefer at this height?"
    SnowballQuery {
        height: u64,
        /// The querier's current preference.
        preference: BlockHash,
    },
    /// Snowball response: "I prefer this block."
    SnowballResponse {
        height: u64,
        preference: BlockHash,
    },

    // --- Proof layer ---

    /// Proof challenge issued to a validator.
    ProofChallenge(ProofChallenge),
    /// Proof response from a validator.
    ProofResponse(ProofResponse),

    // --- Peer discovery ---

    /// Request peer list from a node.
    PeerRequest,
    /// Response with known peers.
    PeerResponse(Vec<PeerInfo>),

    /// Ping for liveness and latency measurement.
    Ping { timestamp_ms: u64 },
    /// Pong response with original timestamp for RTT calculation.
    Pong { ping_timestamp_ms: u64, pong_timestamp_ms: u64 },

    // --- Item 108: Compact block relay ---

    /// Compact block announcement: header + tx hashes (not full tx data).
    /// Receiving nodes check their mempool for matching txs and only
    /// request missing ones via CompactBlockRequest.
    CompactBlock(CompactBlock),

    /// Request missing transactions for a compact block.
    CompactBlockRequest(CompactBlockRequest),

    /// Response with the requested transactions.
    CompactBlockResponse {
        /// Block hash this response is for.
        block_hash: BlockHash,
        /// The requested transactions.
        transactions: Vec<Transaction>,
    },
}

/// Item 108: A compact block contains the header and transaction hashes,
/// allowing peers to reconstruct the full block from their mempool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactBlock {
    /// The full block header.
    pub header: BlockHeader,
    /// Transaction hashes in block order.
    pub tx_hashes: Vec<TxHash>,
}

impl CompactBlock {
    /// Create a CompactBlock from a full Block.
    pub fn from_block(block: &Block) -> Self {
        Self {
            header: block.header.clone(),
            tx_hashes: block.transactions.iter().map(|tx| tx.hash()).collect(),
        }
    }

    /// Block hash (delegated to header).
    pub fn block_hash(&self) -> BlockHash {
        self.header.hash()
    }
}

/// Item 108: Request for missing transactions in a compact block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactBlockRequest {
    /// The block hash we need transactions for.
    pub block_hash: BlockHash,
    /// Indices of missing transactions (positions in the compact block's tx_hashes).
    pub missing_indices: Vec<u32>,
}

/// Peer info shared during discovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfo {
    pub id: PeerId,
    pub address: String,
    pub port: u16,
}
