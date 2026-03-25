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
