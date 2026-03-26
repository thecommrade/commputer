use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

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
    /// Item 106: Bandwidth score — estimated bytes/sec throughput to this peer.
    #[serde(default)]
    pub bandwidth_score: u64,
    /// Item 120: Additional addresses for this peer (IPv4, IPv6, Tailscale, Tor).
    #[serde(default)]
    pub addresses: Vec<String>,
}

// ---------------------------------------------------------------------------
// Item 105: Geographic diversity scorer based on /16 subnet diversity.
// ---------------------------------------------------------------------------

/// Scores the peer set based on /16 subnet diversity.
/// A higher score means better geographic diversity.
pub struct GeoScorer;

impl GeoScorer {
    /// Compute a diversity score for the current peer set.
    /// Returns a value between 0.0 (all peers in one subnet) and 1.0 (perfect diversity).
    pub fn score(peers: &PeerStore) -> f64 {
        let connected: Vec<&PeerInfo> = peers.connected();
        if connected.len() <= 1 {
            return 1.0; // Trivially diverse
        }
        let mut subnet_counts: HashMap<String, usize> = HashMap::new();
        for peer in &connected {
            let subnet = Self::extract_subnet_16(&peer.address);
            *subnet_counts.entry(subnet).or_insert(0) += 1;
        }
        let unique_subnets = subnet_counts.len() as f64;
        let total_peers = connected.len() as f64;
        // Score = unique_subnets / total_peers (1.0 = every peer in a different /16)
        unique_subnets / total_peers
    }

    /// Check if adding a peer from this IP would improve diversity.
    pub fn improves_diversity(ip: &str, peers: &PeerStore) -> bool {
        let subnet = Self::extract_subnet_16(ip);
        let connected = peers.connected();
        let mut subnet_counts: HashMap<String, usize> = HashMap::new();
        for peer in &connected {
            let s = Self::extract_subnet_16(&peer.address);
            *subnet_counts.entry(s).or_insert(0) += 1;
        }
        // If this subnet is not yet represented, or underrepresented, it improves diversity
        let current_count = subnet_counts.get(&subnet).copied().unwrap_or(0);
        let avg = if subnet_counts.is_empty() {
            0
        } else {
            connected.len() / subnet_counts.len()
        };
        current_count <= avg
    }

    fn extract_subnet_16(ip: &str) -> String {
        let ip_part = ip.split(':').next().unwrap_or(ip);
        let octets: Vec<&str> = ip_part.split('.').collect();
        if octets.len() >= 2 {
            format!("{}.{}", octets[0], octets[1])
        } else {
            ip_part.to_string()
        }
    }
}

// ---------------------------------------------------------------------------
// Item 118: Exponential backoff for failed connections.
// ---------------------------------------------------------------------------

/// Tracks exponential backoff for failed peer connections.
/// Backoff doubles on each failure: 1s, 2s, 4s, 8s, ... up to max 5 minutes.
#[derive(Debug, Clone)]
pub struct ConnectionBackoff {
    /// Maps peer address to (next_allowed_ms timestamp, current_backoff_secs).
    backoffs: HashMap<String, (u64, u64)>,
}

impl ConnectionBackoff {
    const INITIAL_BACKOFF_SECS: u64 = 1;
    const MAX_BACKOFF_SECS: u64 = 300; // 5 minutes

    pub fn new() -> Self {
        Self {
            backoffs: HashMap::new(),
        }
    }

    /// Check if we're allowed to connect to this address now.
    pub fn can_connect(&self, addr: &str, now_ms: u64) -> bool {
        match self.backoffs.get(addr) {
            Some((next_allowed, _)) => now_ms >= *next_allowed,
            None => true,
        }
    }

    /// Record a failed connection attempt. Increases the backoff.
    pub fn record_failure(&mut self, addr: &str, now_ms: u64) {
        let (_, current_backoff) = self.backoffs.get(addr).copied().unwrap_or((0, 0));
        let new_backoff = if current_backoff == 0 {
            Self::INITIAL_BACKOFF_SECS
        } else {
            (current_backoff * 2).min(Self::MAX_BACKOFF_SECS)
        };
        let next_allowed = now_ms + new_backoff * 1000;
        self.backoffs.insert(addr.to_string(), (next_allowed, new_backoff));
        debug!(addr, new_backoff, "connection backoff increased");
    }

    /// Record a successful connection. Resets the backoff.
    pub fn record_success(&mut self, addr: &str) {
        self.backoffs.remove(addr);
    }

    /// Get the current backoff duration in seconds for a given address.
    pub fn current_backoff_secs(&self, addr: &str) -> u64 {
        self.backoffs.get(addr).map(|(_, b)| *b).unwrap_or(0)
    }
}

impl Default for ConnectionBackoff {
    fn default() -> Self {
        Self::new()
    }
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

    /// Item 106: Select peers with bandwidth score above the given threshold.
    /// Used for block relay where high-bandwidth peers are preferred.
    pub fn select_high_bandwidth_peers(&self, min_bandwidth: u64) -> Vec<&PeerInfo> {
        let mut peers: Vec<&PeerInfo> = self.peers
            .values()
            .filter(|p| p.connected && p.bandwidth_score >= min_bandwidth)
            .collect();
        peers.sort_by(|a, b| b.bandwidth_score.cmp(&a.bandwidth_score));
        peers
    }

    /// Item 119: Save known peers to a JSON file for persistence across restarts.
    pub fn save_to_file(&self, path: &std::path::Path) -> Result<(), std::io::Error> {
        let peers: Vec<&PeerInfo> = self.peers.values().collect();
        let json = serde_json::to_string_pretty(&peers)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, json)?;
        info!("Saved {} peers to {}", peers.len(), path.display());
        Ok(())
    }

    /// Item 119: Load known peers from a JSON file.
    pub fn load_from_file(path: &std::path::Path) -> Result<Self, std::io::Error> {
        let json = std::fs::read_to_string(path)?;
        let peers: Vec<PeerInfo> = serde_json::from_str(&json)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        let mut store = Self::new();
        for peer in peers {
            store.add(peer);
        }
        info!("Loaded {} peers from {}", store.len(), path.display());
        Ok(store)
    }

    /// Get all known peers (connected and disconnected).
    pub fn all_peers(&self) -> Vec<&PeerInfo> {
        self.peers.values().collect()
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
            bandwidth_score: 0,
            addresses: vec![],
        }
    }

    fn make_peer_with_ip(n: u8, ip: &str, connected: bool) -> PeerInfo {
        let mut id = [0u8; 32];
        id[0] = n;
        PeerInfo {
            id: PeerId(id),
            address: ip.to_string(),
            port: 9000 + n as u16,
            last_seen_ms: 1000,
            avg_rtt_ms: None,
            connected,
            bandwidth_score: 0,
            addresses: vec![],
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

    // Item 105: GeoScorer tests
    #[test]
    fn geo_scorer_perfect_diversity() {
        let mut store = PeerStore::new();
        store.add(make_peer_with_ip(1, "10.0.1.1", true));
        store.add(make_peer_with_ip(2, "10.1.1.1", true));
        store.add(make_peer_with_ip(3, "10.2.1.1", true));
        let score = GeoScorer::score(&store);
        assert!((score - 1.0).abs() < f64::EPSILON, "perfect diversity should score 1.0");
    }

    #[test]
    fn geo_scorer_low_diversity() {
        let mut store = PeerStore::new();
        store.add(make_peer_with_ip(1, "10.0.1.1", true));
        store.add(make_peer_with_ip(2, "10.0.1.2", true));
        store.add(make_peer_with_ip(3, "10.0.1.3", true));
        let score = GeoScorer::score(&store);
        assert!(score < 0.5, "all same /16 should score low: {}", score);
    }

    #[test]
    fn geo_scorer_improves_diversity() {
        let mut store = PeerStore::new();
        store.add(make_peer_with_ip(1, "10.0.1.1", true));
        store.add(make_peer_with_ip(2, "10.0.1.2", true));
        assert!(GeoScorer::improves_diversity("10.1.1.1", &store), "new subnet should improve diversity");
    }

    // Item 106: High bandwidth peer selection tests
    #[test]
    fn select_high_bandwidth_peers() {
        let mut store = PeerStore::new();
        let mut p1 = make_peer(1, true);
        p1.bandwidth_score = 5000;
        store.add(p1);

        let mut p2 = make_peer(2, true);
        p2.bandwidth_score = 100;
        store.add(p2);

        let mut p3 = make_peer(3, true);
        p3.bandwidth_score = 8000;
        store.add(p3);

        let high = store.select_high_bandwidth_peers(1000);
        assert_eq!(high.len(), 2);
        assert_eq!(high[0].bandwidth_score, 8000); // Sorted descending
    }

    // Item 118: Connection backoff tests
    #[test]
    fn connection_backoff_initial() {
        let backoff = ConnectionBackoff::new();
        assert!(backoff.can_connect("1.2.3.4", 0));
    }

    #[test]
    fn connection_backoff_exponential() {
        let mut backoff = ConnectionBackoff::new();
        backoff.record_failure("1.2.3.4", 1000);
        assert_eq!(backoff.current_backoff_secs("1.2.3.4"), 1);
        assert!(!backoff.can_connect("1.2.3.4", 1500)); // Within 1s backoff
        assert!(backoff.can_connect("1.2.3.4", 2001)); // After 1s backoff

        backoff.record_failure("1.2.3.4", 2001);
        assert_eq!(backoff.current_backoff_secs("1.2.3.4"), 2);

        backoff.record_failure("1.2.3.4", 5000);
        assert_eq!(backoff.current_backoff_secs("1.2.3.4"), 4);
    }

    #[test]
    fn connection_backoff_max() {
        let mut backoff = ConnectionBackoff::new();
        // Force many failures
        for i in 0..20 {
            backoff.record_failure("1.2.3.4", i * 1000);
        }
        assert!(backoff.current_backoff_secs("1.2.3.4") <= 300);
    }

    #[test]
    fn connection_backoff_reset_on_success() {
        let mut backoff = ConnectionBackoff::new();
        backoff.record_failure("1.2.3.4", 1000);
        assert!(!backoff.can_connect("1.2.3.4", 1500));
        backoff.record_success("1.2.3.4");
        assert!(backoff.can_connect("1.2.3.4", 1500));
    }

    // Item 119: Peer persistence tests
    #[test]
    fn peer_persistence_roundtrip() {
        let mut store = PeerStore::new();
        store.add(make_peer(1, true));
        store.add(make_peer(2, false));

        let dir = std::env::temp_dir().join("commputer_peer_test");
        let path = dir.join("peers.json");
        store.save_to_file(&path).unwrap();

        let loaded = PeerStore::load_from_file(&path).unwrap();
        assert_eq!(loaded.len(), 2);
        assert!(loaded.get(&store.all_ids()[0]).is_some());

        // Cleanup
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Item 120: Multi-address peer support tests
    #[test]
    fn multi_address_peer() {
        let mut store = PeerStore::new();
        let mut peer = make_peer(1, true);
        peer.addresses = vec![
            "192.168.1.1".to_string(),
            "2001:db8::1".to_string(),
            "100.100.1.1".to_string(), // Tailscale
        ];
        store.add(peer);

        let p = store.get(&PeerId({
            let mut id = [0u8; 32];
            id[0] = 1;
            id
        })).unwrap();
        assert_eq!(p.addresses.len(), 3);
    }
}
