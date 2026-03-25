use std::collections::HashMap;
use serde::{Deserialize, Serialize};

/// A peer identifier — 32 bytes derived from their public key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PeerId(pub [u8; 32]);

impl std::fmt::Display for PeerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "peer:{}", hex::encode(&self.0[..8]))
    }
}

/// Information about a connected or known peer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfo {
    pub id: PeerId,
    pub address: String,
    pub port: u16,
    /// Last seen timestamp (unix ms).
    pub last_seen_ms: u64,
    /// Average round-trip time in ms (for latency triangulation).
    pub avg_rtt_ms: Option<u64>,
    /// Whether this peer is currently connected.
    pub connected: bool,
}

/// Stores known peers and their state.
#[derive(Debug, Default)]
pub struct PeerStore {
    peers: HashMap<PeerId, PeerInfo>,
}

impl PeerStore {
    /// Create an empty peer store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add or update a peer.
    pub fn add(&mut self, info: PeerInfo) {
        self.peers.insert(info.id, info);
    }

    /// Look up a peer by ID.
    pub fn get(&self, id: &PeerId) -> Option<&PeerInfo> {
        self.peers.get(id)
    }

    /// Get a mutable reference to a peer.
    pub fn get_mut(&mut self, id: &PeerId) -> Option<&mut PeerInfo> {
        self.peers.get_mut(id)
    }

    /// Remove a peer from the store.
    pub fn remove(&mut self, id: &PeerId) {
        self.peers.remove(id);
    }

    /// All connected peers.
    pub fn connected(&self) -> Vec<&PeerInfo> {
        self.peers.values().filter(|p| p.connected).collect()
    }

    /// All known peer IDs.
    pub fn all_ids(&self) -> Vec<PeerId> {
        self.peers.keys().copied().collect()
    }

    /// Count of connected peers.
    pub fn connected_count(&self) -> usize {
        self.peers.values().filter(|p| p.connected).count()
    }

    /// Total known peers.
    /// Total known peers.
    pub fn len(&self) -> usize {
        self.peers.len()
    }

    /// Returns true if no peers are known.
    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }

    /// Record a RTT measurement for a peer.
    pub fn record_rtt(&mut self, id: &PeerId, rtt_ms: u64) {
        if let Some(peer) = self.peers.get_mut(id) {
            peer.avg_rtt_ms = Some(match peer.avg_rtt_ms {
                Some(existing) => (existing * 7 + rtt_ms) / 8, // Exponential moving average
                None => rtt_ms,
            });
        }
    }

    /// Prune peers not seen within the given duration (ms).
    pub fn prune_stale(&mut self, now_ms: u64, max_age_ms: u64) {
        self.peers.retain(|_, p| {
            now_ms.saturating_sub(p.last_seen_ms) < max_age_ms
        });
    }

    /// Select random peers for Snowball sampling.
    pub fn random_sample(&self, count: usize, rng: &mut impl rand::Rng) -> Vec<PeerId> {
        use rand::seq::SliceRandom;
        let mut connected: Vec<PeerId> = self.peers
            .iter()
            .filter(|(_, p)| p.connected)
            .map(|(id, _)| *id)
            .collect();
        connected.shuffle(rng);
        connected.truncate(count);
        connected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_peer(n: u8, connected: bool) -> PeerInfo {
        let mut id = [0u8; 32];
        id[0] = n;
        PeerInfo {
            id: PeerId(id),
            address: format!("192.168.1.{}", n),
            port: 9000 + n as u16,
            last_seen_ms: 1000,
            avg_rtt_ms: None,
            connected,
        }
    }

    #[test]
    fn add_and_retrieve() {
        let mut store = PeerStore::new();
        let peer = make_peer(1, true);
        let id = peer.id;
        store.add(peer);

        assert!(store.get(&id).is_some());
        assert_eq!(store.connected_count(), 1);
    }

    #[test]
    fn rtt_tracking() {
        let mut store = PeerStore::new();
        let peer = make_peer(1, true);
        let id = peer.id;
        store.add(peer);

        store.record_rtt(&id, 100);
        assert_eq!(store.get(&id).unwrap().avg_rtt_ms, Some(100));

        // EMA: (100 * 7 + 50) / 8 = 93
        store.record_rtt(&id, 50);
        assert_eq!(store.get(&id).unwrap().avg_rtt_ms, Some(93));
    }

    #[test]
    fn prune_stale_peers() {
        let mut store = PeerStore::new();
        let mut old = make_peer(1, true);
        old.last_seen_ms = 100;
        store.add(old);

        let mut fresh = make_peer(2, true);
        fresh.last_seen_ms = 900;
        store.add(fresh);

        store.prune_stale(1000, 500); // Prune anything older than 500ms
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn random_sample_respects_count() {
        let mut store = PeerStore::new();
        for i in 0..10 {
            store.add(make_peer(i, true));
        }

        let mut rng = rand::thread_rng();
        let sample = store.random_sample(3, &mut rng);
        assert_eq!(sample.len(), 3);
    }
}
