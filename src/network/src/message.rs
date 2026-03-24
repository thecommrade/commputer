use serde::{Deserialize, Serialize};
use commputer_core::block::{Block, BlockHash};
use commputer_core::transaction::Transaction;
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
}

/// Peer info shared during discovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfo {
    pub id: PeerId,
    pub address: String,
    pub port: u16,
}
