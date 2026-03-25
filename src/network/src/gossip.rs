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
}
