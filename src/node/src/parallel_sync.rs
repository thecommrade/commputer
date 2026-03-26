#![allow(dead_code)]
//! Item 109: Parallel block download during initial sync.
//!
//! Requests blocks from multiple peers simultaneously to speed up
//! initial blockchain synchronization. Splits the block range into
//! chunks and assigns each chunk to a different peer.

use std::collections::HashMap;
use tracing::{info, warn, debug};

/// Status of a block request to a peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChunkStatus {
    /// Chunk has been assigned but not yet requested.
    Pending,
    /// Request sent, waiting for response.
    InFlight,
    /// Successfully received all blocks in this chunk.
    Complete,
    /// Failed — needs reassignment to another peer.
    Failed(String),
}

/// A chunk of blocks assigned to a specific peer for download.
#[derive(Debug, Clone)]
pub struct SyncChunk {
    /// Start height (inclusive).
    pub start_height: u64,
    /// End height (inclusive).
    pub end_height: u64,
    /// Peer assigned to download this chunk.
    pub peer_id: String,
    /// Current status.
    pub status: ChunkStatus,
    /// Timestamp when the request was sent (unix ms).
    pub requested_at_ms: Option<u64>,
}

/// Parallel block syncer that coordinates downloading blocks from multiple peers.
pub struct ParallelSyncer {
    /// Our current chain height.
    pub local_height: u64,
    /// Target height we're syncing to.
    pub target_height: u64,
    /// Number of blocks per chunk.
    pub chunk_size: u64,
    /// Active sync chunks.
    pub chunks: Vec<SyncChunk>,
    /// Maximum number of in-flight chunks.
    pub max_in_flight: usize,
    /// Whether sync is active.
    pub active: bool,
    /// Timeout for a chunk request in milliseconds.
    pub chunk_timeout_ms: u64,
}

impl ParallelSyncer {
    /// Default chunk size: 100 blocks per request.
    pub const DEFAULT_CHUNK_SIZE: u64 = 100;
    /// Default max parallel downloads.
    pub const DEFAULT_MAX_IN_FLIGHT: usize = 4;
    /// Default timeout: 30 seconds per chunk.
    pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;

    /// Create a new parallel syncer.
    pub fn new(local_height: u64, target_height: u64) -> Self {
        Self {
            local_height,
            target_height,
            chunk_size: Self::DEFAULT_CHUNK_SIZE,
            chunks: Vec::new(),
            max_in_flight: Self::DEFAULT_MAX_IN_FLIGHT,
            active: false,
            chunk_timeout_ms: Self::DEFAULT_TIMEOUT_MS,
        }
    }

    /// Start the sync process by dividing the range into chunks.
    pub fn start(&mut self, available_peers: &[String]) {
        if available_peers.is_empty() {
            warn!("No peers available for parallel sync");
            return;
        }
        if self.local_height >= self.target_height {
            info!("Already at target height {}", self.target_height);
            return;
        }

        self.active = true;
        self.chunks.clear();

        let mut height = self.local_height + 1;
        let mut peer_index = 0;

        while height <= self.target_height {
            let end = (height + self.chunk_size - 1).min(self.target_height);
            let peer = &available_peers[peer_index % available_peers.len()];
            self.chunks.push(SyncChunk {
                start_height: height,
                end_height: end,
                peer_id: peer.clone(),
                status: ChunkStatus::Pending,
                requested_at_ms: None,
            });
            height = end + 1;
            peer_index += 1;
        }

        info!(
            "Parallel sync: {} chunks across {} peers (heights {} to {})",
            self.chunks.len(),
            available_peers.len(),
            self.local_height + 1,
            self.target_height
        );
    }

    /// Get the next chunks that should be requested (up to max_in_flight).
    pub fn next_requests(&mut self, now_ms: u64) -> Vec<&SyncChunk> {
        let in_flight = self.chunks.iter().filter(|c| c.status == ChunkStatus::InFlight).count();
        let available = self.max_in_flight.saturating_sub(in_flight);

        let mut to_request = Vec::new();
        for chunk in &mut self.chunks {
            if to_request.len() >= available {
                break;
            }
            if chunk.status == ChunkStatus::Pending {
                chunk.status = ChunkStatus::InFlight;
                chunk.requested_at_ms = Some(now_ms);
            }
        }

        // Collect in-flight chunks for return
        for chunk in &self.chunks {
            if chunk.status == ChunkStatus::InFlight && to_request.len() < self.max_in_flight {
                to_request.push(chunk);
            }
        }
        to_request
    }

    /// Mark a chunk as complete.
    pub fn complete_chunk(&mut self, start_height: u64) {
        if let Some(chunk) = self.chunks.iter_mut().find(|c| c.start_height == start_height) {
            chunk.status = ChunkStatus::Complete;
            debug!("Sync chunk complete: heights {} - {}", chunk.start_height, chunk.end_height);
        }

        // Check if all chunks are done
        if self.chunks.iter().all(|c| c.status == ChunkStatus::Complete) {
            self.active = false;
            info!("Parallel sync complete — all blocks downloaded");
        }
    }

    /// Mark a chunk as failed and reassign to another peer if available.
    pub fn fail_chunk(&mut self, start_height: u64, reason: &str, alternate_peer: Option<&str>) {
        if let Some(chunk) = self.chunks.iter_mut().find(|c| c.start_height == start_height) {
            if let Some(peer) = alternate_peer {
                chunk.peer_id = peer.to_string();
                chunk.status = ChunkStatus::Pending;
                chunk.requested_at_ms = None;
                debug!(
                    "Reassigned failed chunk (heights {} - {}) to peer {}",
                    chunk.start_height, chunk.end_height, peer
                );
            } else {
                chunk.status = ChunkStatus::Failed(reason.to_string());
                warn!(
                    "Sync chunk failed: heights {} - {}: {}",
                    chunk.start_height, chunk.end_height, reason
                );
            }
        }
    }

    /// Check for timed-out chunks and mark them as failed.
    pub fn check_timeouts(&mut self, now_ms: u64) -> Vec<u64> {
        let mut timed_out = Vec::new();
        for chunk in &mut self.chunks {
            if chunk.status == ChunkStatus::InFlight {
                if let Some(requested_at) = chunk.requested_at_ms {
                    if now_ms.saturating_sub(requested_at) > self.chunk_timeout_ms {
                        chunk.status = ChunkStatus::Failed("timeout".to_string());
                        timed_out.push(chunk.start_height);
                    }
                }
            }
        }
        timed_out
    }

    /// Progress as a fraction (0.0 to 1.0).
    pub fn progress(&self) -> f64 {
        if self.chunks.is_empty() {
            return 0.0;
        }
        let complete = self.chunks.iter().filter(|c| c.status == ChunkStatus::Complete).count();
        complete as f64 / self.chunks.len() as f64
    }

    /// Whether sync is still active.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Number of blocks remaining to sync.
    pub fn blocks_remaining(&self) -> u64 {
        self.chunks.iter()
            .filter(|c| c.status != ChunkStatus::Complete)
            .map(|c| c.end_height - c.start_height + 1)
            .sum()
    }
}

// ---------------------------------------------------------------------------
// Item 110: Peer heartbeat protocol.
// ---------------------------------------------------------------------------

/// Heartbeat manager for periodic ping/pong and peer liveness detection.
pub struct HeartbeatManager {
    /// Interval between heartbeats in milliseconds.
    pub interval_ms: u64,
    /// Timeout: disconnect peers that don't respond within this many ms.
    pub timeout_ms: u64,
    /// Last ping sent time per peer.
    pub last_ping_sent: HashMap<String, u64>,
    /// Last pong received time per peer.
    pub last_pong_received: HashMap<String, u64>,
    /// Peers that should be disconnected due to timeout.
    pub timed_out_peers: Vec<String>,
}

impl HeartbeatManager {
    /// Default heartbeat interval: 10 seconds.
    pub const DEFAULT_INTERVAL_MS: u64 = 10_000;
    /// Default timeout: 30 seconds.
    pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;

    pub fn new() -> Self {
        Self {
            interval_ms: Self::DEFAULT_INTERVAL_MS,
            timeout_ms: Self::DEFAULT_TIMEOUT_MS,
            last_ping_sent: HashMap::new(),
            last_pong_received: HashMap::new(),
            timed_out_peers: Vec::new(),
        }
    }

    /// Get peers that need a heartbeat ping right now.
    pub fn peers_needing_ping(&self, now_ms: u64, connected_peers: &[String]) -> Vec<String> {
        connected_peers.iter()
            .filter(|peer| {
                let last_sent = self.last_ping_sent.get(*peer).copied().unwrap_or(0);
                now_ms.saturating_sub(last_sent) >= self.interval_ms
            })
            .cloned()
            .collect()
    }

    /// Record that we sent a ping to a peer.
    pub fn record_ping_sent(&mut self, peer: &str, now_ms: u64) {
        self.last_ping_sent.insert(peer.to_string(), now_ms);
    }

    /// Record that we received a pong from a peer.
    pub fn record_pong_received(&mut self, peer: &str, now_ms: u64) {
        self.last_pong_received.insert(peer.to_string(), now_ms);
    }

    /// Check for timed-out peers (sent ping but no pong within timeout).
    /// Returns list of peers that should be disconnected.
    pub fn check_timeouts(&mut self, now_ms: u64) -> Vec<String> {
        self.timed_out_peers.clear();
        for (peer, last_ping) in &self.last_ping_sent {
            let last_pong = self.last_pong_received.get(peer).copied().unwrap_or(0);
            // If we sent a ping and haven't received a pong since, check timeout
            if *last_ping > last_pong && now_ms.saturating_sub(*last_ping) > self.timeout_ms {
                self.timed_out_peers.push(peer.clone());
            }
        }
        self.timed_out_peers.clone()
    }

    /// Remove a peer from tracking (e.g., when disconnected).
    pub fn remove_peer(&mut self, peer: &str) {
        self.last_ping_sent.remove(peer);
        self.last_pong_received.remove(peer);
    }

    /// Get the measured RTT for a peer (last pong - last ping).
    pub fn rtt_ms(&self, peer: &str) -> Option<u64> {
        let ping = self.last_ping_sent.get(peer)?;
        let pong = self.last_pong_received.get(peer)?;
        if *pong > *ping {
            Some(pong - ping)
        } else {
            None
        }
    }
}

impl Default for HeartbeatManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parallel_sync_creates_chunks() {
        let mut syncer = ParallelSyncer::new(0, 500);
        syncer.start(&["peer1".into(), "peer2".into(), "peer3".into()]);

        assert!(syncer.is_active());
        assert_eq!(syncer.chunks.len(), 5); // 500 / 100 = 5 chunks
        assert_eq!(syncer.blocks_remaining(), 500);
    }

    #[test]
    fn parallel_sync_progress() {
        let mut syncer = ParallelSyncer::new(0, 200);
        syncer.start(&["peer1".into()]);

        assert!((syncer.progress() - 0.0).abs() < f64::EPSILON);
        syncer.complete_chunk(1);
        assert!(syncer.progress() > 0.0);
    }

    #[test]
    fn parallel_sync_completes() {
        let mut syncer = ParallelSyncer::new(0, 200);
        syncer.start(&["peer1".into()]);

        for chunk in syncer.chunks.clone() {
            syncer.complete_chunk(chunk.start_height);
        }
        assert!(!syncer.is_active());
    }

    #[test]
    fn parallel_sync_fail_and_reassign() {
        let mut syncer = ParallelSyncer::new(0, 100);
        syncer.start(&["peer1".into()]);

        syncer.fail_chunk(1, "timeout", Some("peer2"));
        let chunk = &syncer.chunks[0];
        assert_eq!(chunk.peer_id, "peer2");
        assert_eq!(chunk.status, ChunkStatus::Pending);
    }

    #[test]
    fn parallel_sync_timeout_detection() {
        let mut syncer = ParallelSyncer::new(0, 100);
        syncer.chunk_timeout_ms = 5000;
        syncer.start(&["peer1".into()]);

        // Manually set to in-flight
        syncer.chunks[0].status = ChunkStatus::InFlight;
        syncer.chunks[0].requested_at_ms = Some(1000);

        let timed_out = syncer.check_timeouts(7000);
        assert_eq!(timed_out.len(), 1);
    }

    #[test]
    fn heartbeat_ping_needed() {
        let hb = HeartbeatManager::new();
        let peers = vec!["peer1".to_string(), "peer2".to_string()];
        let need_ping = hb.peers_needing_ping(20_000, &peers);
        assert_eq!(need_ping.len(), 2);
    }

    #[test]
    fn heartbeat_timeout_detection() {
        let mut hb = HeartbeatManager::new();
        hb.record_ping_sent("peer1", 1000);
        // No pong received

        let timed_out = hb.check_timeouts(35_000);
        assert_eq!(timed_out.len(), 1);
        assert_eq!(timed_out[0], "peer1");
    }

    #[test]
    fn heartbeat_no_timeout_if_pong_received() {
        let mut hb = HeartbeatManager::new();
        hb.record_ping_sent("peer1", 1000);
        hb.record_pong_received("peer1", 1500);

        let timed_out = hb.check_timeouts(35_000);
        assert!(timed_out.is_empty());
    }

    #[test]
    fn heartbeat_rtt_measurement() {
        let mut hb = HeartbeatManager::new();
        hb.record_ping_sent("peer1", 1000);
        hb.record_pong_received("peer1", 1050);

        assert_eq!(hb.rtt_ms("peer1"), Some(50));
    }

    #[test]
    fn parallel_sync_no_peers() {
        let mut syncer = ParallelSyncer::new(0, 100);
        syncer.start(&[]);
        assert!(!syncer.is_active());
    }

    #[test]
    fn parallel_sync_already_synced() {
        let mut syncer = ParallelSyncer::new(100, 100);
        syncer.start(&["peer1".into()]);
        assert!(!syncer.is_active());
    }
}
