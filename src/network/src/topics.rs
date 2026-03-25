use libp2p::gossipsub::IdentTopic;

/// Gossipsub topic for new block announcements.
pub const TOPIC_BLOCKS: &str = "commputer/blocks/0.1";
/// Gossipsub topic for new transactions.
pub const TOPIC_TRANSACTIONS: &str = "commputer/txs/0.1";
/// Gossipsub topic for Snowball consensus messages.
pub const TOPIC_CONSENSUS: &str = "commputer/consensus/0.1";
/// Gossipsub topic for proof challenges and responses.
pub const TOPIC_PROOFS: &str = "commputer/proofs/0.1";
/// Feature 6: Gossipsub topic for peer address exchange.
pub const TOPIC_PEER_ADDRS: &str = "commputer/peer_addrs/0.1";

/// Returns the block announcement topic.
pub fn block_topic() -> IdentTopic { IdentTopic::new(TOPIC_BLOCKS) }
/// Returns the transaction broadcast topic.
pub fn tx_topic() -> IdentTopic { IdentTopic::new(TOPIC_TRANSACTIONS) }
/// Returns the consensus message topic.
pub fn consensus_topic() -> IdentTopic { IdentTopic::new(TOPIC_CONSENSUS) }
/// Returns the proof challenge/response topic.
pub fn proof_topic() -> IdentTopic { IdentTopic::new(TOPIC_PROOFS) }
/// Returns the peer address gossip topic.
pub fn peer_addrs_topic() -> IdentTopic { IdentTopic::new(TOPIC_PEER_ADDRS) }

/// Returns all gossipsub topics the node should subscribe to.
pub fn all_topics() -> Vec<IdentTopic> {
    vec![block_topic(), tx_topic(), consensus_topic(), proof_topic(), peer_addrs_topic()]
}
