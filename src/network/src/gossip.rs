use std::collections::HashSet;
use crate::message::{NetworkMessage, MessageKind};
use crate::peer::{PeerId, PeerStore};
use tracing::debug;

/// Gossip protocol router.
/// Handles message deduplication and fan-out to connected peers.
/// In production this will use libp2p gossipsub; for now it's a
/// clean abstraction layer that the libp2p integration plugs into.
pub struct GossipRouter {
    /// Messages we've already seen (by hash), for deduplication.
    seen: HashSet<u64>,
    /// Maximum seen cache size before pruning.
    max_seen: usize,
    /// Fan-out: how many peers to forward each message to.
    fanout: usize,
}

impl GossipRouter {
    /// Create a router with default fanout (8) and seen cache (100k).
    pub fn new() -> Self {
        Self {
            seen: HashSet::new(),
            max_seen: 100_000,
            fanout: 8,
        }
    }

    /// Set the fan-out count (peers to forward each message to).
    pub fn with_fanout(mut self, fanout: usize) -> Self {
        self.fanout = fanout;
        self
    }

    /// Process an incoming message. Returns true if it's new (not seen before).
    pub fn receive(&mut self, msg: &NetworkMessage) -> bool {
        if self.seen.contains(&msg.nonce) {
            return false;
        }

        self.seen.insert(msg.nonce);

        // Prune seen cache if it's getting large.
        if self.seen.len() > self.max_seen {
            self.prune_seen();
        }

        true
    }

    /// Select peers to forward a message to.
    pub fn select_forward_peers(
        &self,
        sender: &PeerId,
        peers: &PeerStore,
        rng: &mut impl rand::Rng,
    ) -> Vec<PeerId> {
        let mut candidates = peers.random_sample(self.fanout + 1, rng);
        // Don't forward back to the sender.
        candidates.retain(|id| id != sender);
        candidates.truncate(self.fanout);
        candidates
    }

    /// Determine message priority for processing order.
    pub fn priority(kind: &MessageKind) -> MessagePriority {
        match kind {
            MessageKind::SnowballQuery { .. } => MessagePriority::High,
            MessageKind::SnowballResponse { .. } => MessagePriority::High,
            MessageKind::NewBlock(_) => MessagePriority::High,
            MessageKind::CompactBlock(_) => MessagePriority::High,
            MessageKind::CompactBlockRequest(_) => MessagePriority::High,
            MessageKind::CompactBlockResponse { .. } => MessagePriority::High,
            MessageKind::ProofChallenge(_) => MessagePriority::Medium,
            MessageKind::ProofResponse(_) => MessagePriority::Medium,
            MessageKind::NewTransaction(_) => MessagePriority::Normal,
            MessageKind::Ping { .. } => MessagePriority::Low,
            MessageKind::Pong { .. } => MessagePriority::Low,
            MessageKind::PeerRequest => MessagePriority::Low,
            MessageKind::PeerResponse(_) => MessagePriority::Low,
        }
    }

    fn prune_seen(&mut self) {
        // Simple strategy: clear half the cache.
        // In production, use an LRU or time-based eviction.
        let to_remove: Vec<u64> = self.seen.iter().take(self.max_seen / 2).copied().collect();
        for nonce in to_remove {
            self.seen.remove(&nonce);
        }
        debug!(remaining = self.seen.len(), "pruned gossip seen cache");
    }
}

impl Default for GossipRouter {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MessagePriority {
    Low = 0,
    Normal = 1,
    Medium = 2,
    High = 3,
}

// ---------------------------------------------------------------------------
// Item 107: Priority send queue — orders messages by priority before sending.
// ---------------------------------------------------------------------------

/// A queued message with its priority for ordering.
#[derive(Debug)]
struct QueuedMessage {
    priority: MessagePriority,
    data: Vec<u8>,
    topic: String,
    /// Insertion order for FIFO within same priority.
    seq: u64,
}

/// Orders messages by priority (High first) before sending via gossipsub.
/// Within the same priority level, messages are sent in FIFO order.
pub struct PrioritySendQueue {
    queue: Vec<QueuedMessage>,
    next_seq: u64,
}

impl PrioritySendQueue {
    pub fn new() -> Self {
        Self {
            queue: Vec::new(),
            next_seq: 0,
        }
    }

    /// Enqueue a message with the given priority.
    pub fn enqueue(&mut self, data: Vec<u8>, topic: String, priority: MessagePriority) {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.queue.push(QueuedMessage { priority, data, topic, seq });
    }

    /// Drain messages in priority order (highest first, then FIFO).
    /// Returns (topic, data) pairs ready for gossipsub publish.
    pub fn drain_ordered(&mut self) -> Vec<(String, Vec<u8>)> {
        // Sort: higher priority first, then lower seq first (FIFO within priority).
        self.queue.sort_by(|a, b| {
            b.priority.cmp(&a.priority).then(a.seq.cmp(&b.seq))
        });
        let result: Vec<(String, Vec<u8>)> = self.queue
            .drain(..)
            .map(|m| (m.topic, m.data))
            .collect();
        self.next_seq = 0;
        result
    }

    /// Number of pending messages.
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// Whether the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

impl Default for PrioritySendQueue {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{NetworkMessage, MessageKind};
    use crate::peer::PeerId;
    use commputer_core::block::BlockHash;

    fn make_msg(nonce: u64) -> NetworkMessage {
        NetworkMessage {
            sender: PeerId([0u8; 32]),
            nonce,
            kind: MessageKind::Ping { timestamp_ms: 0 },
        }
    }

    #[test]
    fn deduplicates_messages() {
        let mut router = GossipRouter::new();
        let msg = make_msg(42);

        assert!(router.receive(&msg));  // First time: new
        assert!(!router.receive(&msg)); // Second time: duplicate
    }

    #[test]
    fn different_nonces_are_unique() {
        let mut router = GossipRouter::new();
        assert!(router.receive(&make_msg(1)));
        assert!(router.receive(&make_msg(2)));
        assert!(router.receive(&make_msg(3)));
    }

    #[test]
    fn priority_ordering() {
        assert!(
            GossipRouter::priority(&MessageKind::SnowballQuery {
                height: 0,
                preference: BlockHash::GENESIS,
            }) > GossipRouter::priority(&MessageKind::Ping { timestamp_ms: 0 })
        );
    }

    #[test]
    fn blocks_get_high_priority() {
        use commputer_core::block::{Block, BlockHeader};
        use commputer_core::identity::Address;

        let block = Block {
            header: BlockHeader {
                protocol_version: 1,
                height: 1,
                parent_hash: BlockHash::GENESIS,
                tx_root: [0u8; 32],
                proof_root: [0u8; 32],
                state_root: [0u8; 32],
                timestamp: 1000,
                producer: Address([0u8; 32]),
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
        };
        assert_eq!(
            GossipRouter::priority(&MessageKind::NewBlock(block)),
            MessagePriority::High
        );
    }

    #[test]
    fn transactions_get_normal_priority() {
        use commputer_core::transaction::{Transaction, TxKind};
        use commputer_core::identity::Address;
        use commputer_core::token::Amount;

        let tx = Transaction {
            from: Address([0u8; 32]),
            nonce: 0,
            kind: TxKind::Transfer {
                to: Address([1u8; 32]),
                amount: Amount::from_raw(100),
            },
            fee: 1,
            memo: None,
            public_key: vec![0u8; 32],
            signature: vec![0u8; 64],
            timelock: None,
        };
        assert_eq!(
            GossipRouter::priority(&MessageKind::NewTransaction(tx)),
            MessagePriority::Normal
        );
    }

    #[test]
    fn priority_send_queue_ordering() {
        let mut queue = PrioritySendQueue::new();
        queue.enqueue(b"low".to_vec(), "topic".into(), MessagePriority::Low);
        queue.enqueue(b"high".to_vec(), "topic".into(), MessagePriority::High);
        queue.enqueue(b"normal".to_vec(), "topic".into(), MessagePriority::Normal);

        let drained = queue.drain_ordered();
        assert_eq!(drained.len(), 3);
        assert_eq!(&drained[0].1, b"high");
        assert_eq!(&drained[1].1, b"normal");
        assert_eq!(&drained[2].1, b"low");
    }

    #[test]
    fn priority_send_queue_fifo_within_priority() {
        let mut queue = PrioritySendQueue::new();
        queue.enqueue(b"first".to_vec(), "t".into(), MessagePriority::High);
        queue.enqueue(b"second".to_vec(), "t".into(), MessagePriority::High);
        queue.enqueue(b"third".to_vec(), "t".into(), MessagePriority::High);

        let drained = queue.drain_ordered();
        assert_eq!(&drained[0].1, b"first");
        assert_eq!(&drained[1].1, b"second");
        assert_eq!(&drained[2].1, b"third");
    }
}
